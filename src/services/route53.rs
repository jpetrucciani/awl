use aws_sdk_route53::types::{
    Change, ChangeAction, ChangeBatch, ResourceRecord, ResourceRecordSet, RrType,
};

use crate::cli::{Globals, Route53Command, Route53CommandSubcommand, StubArgs};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};
use crate::services::common;

pub async fn run(globals: &Globals, command: &Route53Command) -> Result<()> {
    let context = AwsContext::new(globals).await?;
    let client = context.route53();
    match &command.command {
        Route53CommandSubcommand::Zones(_) => zones(globals, &client).await,
        Route53CommandSubcommand::Ls(args) => list_records(globals, &client, args).await,
        Route53CommandSubcommand::Get(args) => get_record(globals, &client, args).await,
        Route53CommandSubcommand::Set(args) => set_record(globals, &client, args).await,
    }
}

async fn zones(globals: &Globals, client: &aws_sdk_route53::Client) -> Result<()> {
    let response = client
        .list_hosted_zones()
        .send()
        .await
        .map_err(AwlError::aws)?;
    let rows = response
        .hosted_zones()
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

async fn list_records(
    globals: &Globals,
    client: &aws_sdk_route53::Client,
    args: &StubArgs,
) -> Result<()> {
    let zone = resolve_zone_id(
        client,
        &common::required(&common::positional(&args.args), 0, "zone")?,
    )
    .await?;
    let mut request = client
        .list_resource_record_sets()
        .hosted_zone_id(zone.clone());
    if let Some(name) = common::option_value(&args.args, "--name") {
        request = request.start_record_name(ensure_trailing_dot(&name));
    }
    if let Some(record_type) = common::option_value(&args.args, "--type") {
        request = request.start_record_type(RrType::from(record_type.to_uppercase().as_str()));
    }
    let response = request.send().await.map_err(AwlError::aws)?;
    let rows = response
        .resource_record_sets()
        .iter()
        .map(record_row)
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn get_record(
    globals: &Globals,
    client: &aws_sdk_route53::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let zone = resolve_zone_id(client, &common::required(&positional, 0, "zone")?).await?;
    let name = ensure_trailing_dot(&common::required(&positional, 1, "name")?);
    let record_type = common::option_value(&args.args, "--type")
        .or_else(|| positional.get(2).cloned())
        .unwrap_or_else(|| "A".to_owned());
    let record_type = RrType::from(record_type.to_uppercase().as_str());
    let response = client
        .list_resource_record_sets()
        .hosted_zone_id(zone)
        .start_record_name(name.clone())
        .start_record_type(record_type.clone())
        .send()
        .await
        .map_err(AwlError::aws)?;
    let Some(record) = response
        .resource_record_sets()
        .iter()
        .find(|record| record.name() == name && record.r#type().as_str() == record_type.as_str())
    else {
        return Err(AwlError::NotFound {
            message: format!("record {name} {} not found", record_type.as_str()),
        });
    };
    output::emit(globals, Output::one(record_row(record))?)
}

async fn set_record(
    globals: &Globals,
    client: &aws_sdk_route53::Client,
    args: &StubArgs,
) -> Result<()> {
    let positional = common::positional(&args.args);
    let zone = resolve_zone_id(client, &common::required(&positional, 0, "zone")?).await?;
    let name = ensure_trailing_dot(&common::required(&positional, 1, "name")?);
    let record_type = RrType::from(
        common::required(&positional, 2, "type")?
            .to_uppercase()
            .as_str(),
    );
    let values = positional.iter().skip(3).cloned().collect::<Vec<_>>();
    if values.is_empty() {
        return Err(AwlError::Usage {
            message: "route53 set requires at least one record value".to_owned(),
        });
    }
    let ttl = common::option_value(&args.args, "--ttl")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(300);
    let action = common::option_value(&args.args, "--action")
        .map(|value| ChangeAction::from(value.to_uppercase().as_str()))
        .unwrap_or(ChangeAction::Upsert);

    let mut record_set = ResourceRecordSet::builder()
        .name(name.clone())
        .r#type(record_type.clone())
        .ttl(ttl);
    for value in &values {
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
        .map_err(AwlError::aws)?;
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "name": name,
            "type": record_type.as_str(),
            "values": values,
            "ttl": ttl,
            "action": action.as_str(),
            "change_id": response.change_info().map(|info| info.id()),
            "status": response.change_info().map(|info| info.status().as_str()),
        }))?,
    )
}

async fn resolve_zone_id(client: &aws_sdk_route53::Client, zone: &str) -> Result<String> {
    if zone.starts_with("/hostedzone/") || zone.starts_with('Z') {
        return Ok(clean_zone_id(zone).to_owned());
    }

    let wanted = ensure_trailing_dot(zone);
    let response = client
        .list_hosted_zones()
        .send()
        .await
        .map_err(AwlError::aws)?;
    let matches = response
        .hosted_zones()
        .iter()
        .filter(|candidate| candidate.name() == wanted)
        .map(|candidate| {
            format!(
                "{} ({}, private={})",
                clean_zone_id(candidate.id()),
                candidate.name(),
                candidate
                    .config()
                    .is_some_and(|config| config.private_zone())
            )
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [only] => Ok(only
            .split_once(' ')
            .map(|(id, _)| id.to_owned())
            .unwrap_or_else(|| only.clone())),
        [] => Err(AwlError::NotFound {
            message: format!("hosted zone {zone:?} not found"),
        }),
        _ => Err(AwlError::Usage {
            message: format!("hosted zone {zone:?} is ambiguous: {}", matches.join(", ")),
        }),
    }
}

fn record_row(record: &ResourceRecordSet) -> serde_json::Value {
    serde_json::json!({
        "name": record.name(),
        "type": record.r#type().as_str(),
        "ttl": record.ttl(),
        "values": record
            .resource_records()
            .iter()
            .map(|value| value.value())
            .collect::<Vec<_>>(),
    })
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
