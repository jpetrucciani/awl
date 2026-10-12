use aws_sdk_route53::types::{
    Change, ChangeAction, ChangeBatch, ResourceRecord, ResourceRecordSet, RrType,
};

use crate::cli::{
    Globals, Route53Action, Route53Command, Route53LsArgs, Route53SetArgs, Route53Subcommand,
};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

pub async fn run(globals: &Globals, command: &Route53Command) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.route53();
    match &command.command {
        Route53Subcommand::Zones => {
            let rows = hosted_zones(&client)
                .await?
                .iter()
                .map(|zone| {
                    serde_json::json!({
                        "id": clean_zone_id(zone.id()),
                        "name": zone.name(),
                        "private": zone.config().map(|config| config.private_zone()),
                        "record_count": zone.resource_record_set_count(),
                    })
                })
                .collect::<Vec<_>>();
            output::emit(globals, Output::Many(rows))
        }
        Route53Subcommand::Ls(args) => list_records(globals, &client, args).await,
        Route53Subcommand::Get(args) => {
            let zone = resolve_zone_id(&client, &args.zone).await?;
            let name = ensure_trailing_dot(&args.name);
            let record_type = RrType::from(args.record_type.to_uppercase().as_str());
            let records = record_sets(&client, &zone, Some(&name), Some(&record_type)).await?;
            let Some(record) = records.first() else {
                return Err(AwlError::NotFound {
                    message: format!("record {name} {} not found", record_type.as_str()),
                });
            };
            output::emit(globals, Output::one(record_row(record))?)
        }
        Route53Subcommand::Set(args) => set_record(globals, &client, args).await,
    }
}

async fn hosted_zones(
    client: &aws_sdk_route53::Client,
) -> Result<Vec<aws_sdk_route53::types::HostedZone>> {
    let mut zones = Vec::new();
    let mut pages = client.list_hosted_zones().into_paginator().send();
    while let Some(page) = pages.next().await {
        zones.extend(page.map_err(AwlError::sdk)?.hosted_zones().iter().cloned());
    }
    Ok(zones)
}

/// Record sets in a zone, optionally only those matching `name` and `record_type`.
/// ListResourceRecordSets has no SDK paginator, so walk its multi-part token.
async fn record_sets(
    client: &aws_sdk_route53::Client,
    zone: &str,
    name: Option<&str>,
    record_type: Option<&RrType>,
) -> Result<Vec<ResourceRecordSet>> {
    let mut out = Vec::new();
    let mut start_name = name.map(str::to_owned);
    let mut start_type = name.and(record_type.cloned());
    let mut start_identifier = None;
    loop {
        let response = client
            .list_resource_record_sets()
            .hosted_zone_id(zone)
            .set_start_record_name(start_name.take())
            .set_start_record_type(start_type.take())
            .set_start_record_identifier(start_identifier.take())
            .send()
            .await
            .map_err(AwlError::sdk)?;
        for record in response.resource_record_sets() {
            if let Some(name) = name
                && !same_name(record.name(), name)
            {
                // Results are sorted by name, so the requested name is done.
                return Ok(out);
            }
            if record_type.is_none_or(|wanted| record.r#type() == wanted) {
                out.push(record.clone());
            }
        }
        if !response.is_truncated() {
            return Ok(out);
        }
        start_name = response.next_record_name().map(str::to_owned);
        start_type = response.next_record_type().cloned();
        start_identifier = response.next_record_identifier().map(str::to_owned);
    }
}

async fn list_records(
    globals: &Globals,
    client: &aws_sdk_route53::Client,
    args: &Route53LsArgs,
) -> Result<()> {
    let zone = resolve_zone_id(client, &args.zone).await?;
    let name = args.name.as_deref().map(ensure_trailing_dot);
    let record_type = args
        .record_type
        .as_deref()
        .map(|value| RrType::from(value.to_uppercase().as_str()));
    let rows = record_sets(client, &zone, name.as_deref(), record_type.as_ref())
        .await?
        .iter()
        .map(record_row)
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn set_record(
    globals: &Globals,
    client: &aws_sdk_route53::Client,
    args: &Route53SetArgs,
) -> Result<()> {
    let zone = resolve_zone_id(client, &args.zone).await?;
    let name = ensure_trailing_dot(&args.name);
    let record_type = RrType::from(args.record_type.to_uppercase().as_str());
    let action = match args.action {
        Route53Action::Create => ChangeAction::Create,
        Route53Action::Upsert => ChangeAction::Upsert,
        Route53Action::Delete => ChangeAction::Delete,
    };

    let mut record_set = ResourceRecordSet::builder()
        .name(name.clone())
        .r#type(record_type.clone())
        .ttl(args.ttl);
    for value in &args.values {
        record_set = record_set.resource_records(
            ResourceRecord::builder()
                .value(value)
                .build()
                .map_err(AwlError::aws)?,
        );
    }
    let change = Change::builder()
        .action(action.clone())
        .resource_record_set(record_set.build().map_err(AwlError::aws)?)
        .build()
        .map_err(AwlError::aws)?;
    let batch = ChangeBatch::builder()
        .changes(change)
        .build()
        .map_err(AwlError::aws)?;
    let response = client
        .change_resource_record_sets()
        .hosted_zone_id(zone)
        .change_batch(batch)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    output::ack(
        globals,
        Output::one(serde_json::json!({
            "name": name,
            "type": record_type.as_str(),
            "values": args.values,
            "ttl": args.ttl,
            "action": action.as_str(),
            "change_id": response.change_info().map(|info| info.id()),
            "status": response.change_info().map(|info| info.status().as_str()),
        }))?,
    )
}

/// Accept a zone ID (`Z123`, `/hostedzone/Z123`) or a zone name (anything with a dot).
async fn resolve_zone_id(client: &aws_sdk_route53::Client, zone: &str) -> Result<String> {
    if zone.starts_with("/hostedzone/") || !zone.contains('.') {
        return Ok(clean_zone_id(zone).to_owned());
    }

    let wanted = ensure_trailing_dot(zone);
    let matches = hosted_zones(client)
        .await?
        .into_iter()
        .filter(|candidate| same_name(candidate.name(), &wanted))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [only] => Ok(clean_zone_id(only.id()).to_owned()),
        [] => Err(AwlError::NotFound {
            message: format!("hosted zone {zone:?} not found"),
        }),
        _ => Err(AwlError::Usage {
            message: format!(
                "hosted zone {zone:?} is ambiguous: {}; pass a zone ID",
                matches
                    .iter()
                    .map(|candidate| format!(
                        "{} (private={})",
                        clean_zone_id(candidate.id()),
                        candidate
                            .config()
                            .is_some_and(|config| config.private_zone())
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

fn record_row(record: &ResourceRecordSet) -> serde_json::Value {
    serde_json::json!({
        "name": unescape_name(record.name()),
        "type": record.r#type().as_str(),
        "ttl": record.ttl(),
        "values": record
            .resource_records()
            .iter()
            .map(|value| value.value())
            .collect::<Vec<_>>(),
        "alias": record.alias_target().map(|alias| alias.dns_name()),
    })
}

/// Route53 returns lowercase names with `*` escaped as `\052`.
fn same_name(returned: &str, wanted: &str) -> bool {
    unescape_name(returned).eq_ignore_ascii_case(wanted)
}

fn unescape_name(value: &str) -> String {
    value.replace("\\052", "*")
}

fn ensure_trailing_dot(value: &str) -> String {
    if value.ends_with('.') {
        value.to_owned()
    } else {
        format!("{value}.")
    }
}

fn clean_zone_id(value: &str) -> &str {
    value.strip_prefix("/hostedzone/").unwrap_or(value)
}
