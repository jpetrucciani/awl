use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::process::Command;

use aws_sdk_ssm::config::endpoint::{Params, ResolveEndpoint};
use serde::Deserialize;

use crate::cli::{
    Ec2AmiArgs, Ec2Command, Ec2InstancesArgs, Ec2LsArgs, Ec2StopArgs, Ec2Subcommand, Ec2TypesArgs,
    Ec2TypesSort, Globals,
};
use crate::client::AwsContext;
use crate::error::{AwlError, Result};
use crate::output::{self, Output};

const EC2_TYPES_JSON: &str = include_str!("../../data/ec2_instance_types.json");

pub async fn run(globals: &Globals, command: &Ec2Command) -> Result<()> {
    if let Ec2Subcommand::Types(args) = &command.command {
        return types(globals, args);
    }

    let context = AwsContext::new(globals).await?;
    let client = context.ec2();
    match &command.command {
        Ec2Subcommand::Ls(args) => ls(globals, &client, args).await,
        Ec2Subcommand::Start(args) => start(globals, &client, args).await,
        Ec2Subcommand::Stop(args) => stop(globals, &client, args).await,
        Ec2Subcommand::Reboot(args) => reboot(globals, &client, args).await,
        Ec2Subcommand::Console(args) => console(globals, &client, &args.instance_id).await,
        Ec2Subcommand::Ssh(args) => ssh(&context, &args.instance_id, globals).await,
        Ec2Subcommand::Ami(args) => ami(globals, &client, args).await,
        Ec2Subcommand::Types(args) => types(globals, args),
    }
}

fn types(globals: &Globals, args: &Ec2TypesArgs) -> Result<()> {
    let catalog: Ec2TypesCatalog = serde_json::from_str(EC2_TYPES_JSON)?;
    let price_region = price_region(globals, args);
    let mut rows = catalog
        .instances
        .iter()
        .filter(|instance| matches_type_filters(instance, args))
        .collect::<Vec<_>>();

    sort_types(&mut rows, args.sort, &price_region);

    let rows = rows
        .into_iter()
        .take(args.limit.unwrap_or(usize::MAX))
        .map(|instance| type_row(instance, args, &price_region))
        .collect::<Vec<_>>();
    output::emit(globals, Output::Many(rows))
}

async fn ls(globals: &Globals, client: &aws_sdk_ec2::Client, args: &Ec2LsArgs) -> Result<()> {
    let mut rows = Vec::new();
    let mut pages = client
        .describe_instances()
        .set_instance_ids((!args.instance_ids.is_empty()).then(|| args.instance_ids.clone()))
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(AwlError::sdk)?;
        rows.extend(
            page.reservations()
                .iter()
                .flat_map(|reservation| reservation.instances())
                .map(instance_row),
        );
    }
    output::emit(globals, Output::Many(rows))
}

async fn start(
    globals: &Globals,
    client: &aws_sdk_ec2::Client,
    args: &Ec2InstancesArgs,
) -> Result<()> {
    let response = client
        .start_instances()
        .set_instance_ids(Some(args.instance_ids.clone()))
        .dry_run(args.dry_run.dry_run)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    output::ack(
        globals,
        Output::Many(state_changes(response.starting_instances())),
    )
}

async fn stop(globals: &Globals, client: &aws_sdk_ec2::Client, args: &Ec2StopArgs) -> Result<()> {
    let response = client
        .stop_instances()
        .set_instance_ids(Some(args.instances.instance_ids.clone()))
        .force(args.force)
        .dry_run(args.instances.dry_run.dry_run)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    output::ack(
        globals,
        Output::Many(state_changes(response.stopping_instances())),
    )
}

async fn reboot(
    globals: &Globals,
    client: &aws_sdk_ec2::Client,
    args: &Ec2InstancesArgs,
) -> Result<()> {
    client
        .reboot_instances()
        .set_instance_ids(Some(args.instance_ids.clone()))
        .dry_run(args.dry_run.dry_run)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    output::ack(
        globals,
        Output::Many(
            args.instance_ids
                .iter()
                .map(|id| serde_json::json!({ "instance_id": id, "action": "reboot" }))
                .collect(),
        ),
    )
}

async fn console(globals: &Globals, client: &aws_sdk_ec2::Client, instance_id: &str) -> Result<()> {
    let response = client
        .get_console_output()
        .instance_id(instance_id)
        .latest(true)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    // The API returns base64; decode it so the console log is readable.
    let output = response.output().map(|encoded| {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_else(|_| encoded.to_owned())
    });
    output::emit(
        globals,
        Output::one(serde_json::json!({
            "instance_id": response.instance_id(),
            "timestamp": response.timestamp().map(ToString::to_string),
            "output": output,
        }))?,
    )
}

async fn ssh(context: &AwsContext, instance_id: &str, globals: &Globals) -> Result<()> {
    let region = context.region().unwrap_or("us-east-1").to_owned();
    // The plugin needs the same SSM endpoint the session was started against.
    let endpoint = match context.endpoint_url() {
        Some(endpoint) => endpoint.to_owned(),
        None => {
            let params = Params::builder()
                .region(region.clone())
                .build()
                .map_err(AwlError::aws)?;
            aws_sdk_ssm::config::endpoint::DefaultResolver::new()
                .resolve_endpoint(&params)
                .await
                .map_err(AwlError::aws)?
                .url()
                .to_owned()
        }
    };
    let response = context
        .ssm()
        .start_session()
        .target(instance_id)
        .send()
        .await
        .map_err(AwlError::sdk)?;
    let session = serde_json::json!({
        "SessionId": response.session_id(),
        "TokenValue": response.token_value(),
        "StreamUrl": response.stream_url(),
    })
    .to_string();
    let parameters = serde_json::json!({ "Target": instance_id }).to_string();
    let status = Command::new("session-manager-plugin")
        .args([
            session,
            region,
            "StartSession".to_owned(),
            globals.profile.clone().unwrap_or_default(),
            parameters,
            endpoint,
        ])
        .status()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => AwlError::MissingDependency {
                message: "`awl ec2 ssh` needs session-manager-plugin on PATH".to_owned(),
            },
            _ => AwlError::Io(error),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(AwlError::Aws {
            message: format!("session-manager-plugin exited with status {status}"),
        })
    }
}

async fn ami(globals: &Globals, client: &aws_sdk_ec2::Client, args: &Ec2AmiArgs) -> Result<()> {
    // Without IDs or owners DescribeImages walks every public AMI.
    let owners = if args.owner.is_empty() && args.image_ids.is_empty() {
        vec!["self".to_owned()]
    } else {
        args.owner.clone()
    };
    let mut rows = Vec::new();
    let mut pages = client
        .describe_images()
        .set_image_ids((!args.image_ids.is_empty()).then(|| args.image_ids.clone()))
        .set_owners((!owners.is_empty()).then_some(owners))
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        rows.extend(page.map_err(AwlError::sdk)?.images().iter().map(|image| {
            serde_json::json!({
                "image_id": image.image_id(),
                "name": image.name(),
                "description": image.description(),
                "state": image.state().map(|state| state.as_str()),
                "owner_id": image.owner_id(),
                "creation_date": image.creation_date(),
            })
        }));
    }
    output::emit(globals, Output::Many(rows))
}

fn instance_row(instance: &aws_sdk_ec2::types::Instance) -> serde_json::Value {
    serde_json::json!({
        "instance_id": instance.instance_id(),
        "name": tag_value(instance.tags(), "Name"),
        "state": instance
            .state()
            .and_then(|state| state.name())
            .map(|name| name.as_str()),
        "instance_type": instance.instance_type().map(|value| value.as_str()),
        "private_ip": instance.private_ip_address(),
        "public_ip": instance.public_ip_address(),
        "az": instance.placement().and_then(|placement| placement.availability_zone()),
        "image_id": instance.image_id(),
        "key_name": instance.key_name(),
        "launch_time": instance.launch_time().map(ToString::to_string),
    })
}

fn state_changes(changes: &[aws_sdk_ec2::types::InstanceStateChange]) -> Vec<serde_json::Value> {
    changes
        .iter()
        .map(|change| {
            serde_json::json!({
                "instance_id": change.instance_id(),
                "previous_state": change
                    .previous_state()
                    .and_then(|state| state.name())
                    .map(|name| name.as_str()),
                "current_state": change
                    .current_state()
                    .and_then(|state| state.name())
                    .map(|name| name.as_str()),
            })
        })
        .collect()
}

fn tag_value<'a>(tags: &'a [aws_sdk_ec2::types::Tag], key: &str) -> Option<&'a str> {
    tags.iter()
        .find(|tag| tag.key() == Some(key))
        .and_then(|tag| tag.value())
}

#[derive(Debug, Deserialize)]
struct Ec2TypesCatalog {
    instances: Vec<Ec2TypeRecord>,
}

#[derive(Debug, Deserialize)]
struct Ec2TypeRecord {
    instance_type: String,
    family: Option<String>,
    pretty_name: Option<String>,
    vcpu: Option<u32>,
    memory_gib: Option<f64>,
    #[serde(default)]
    arch: Vec<String>,
    generation: Option<String>,
    network_performance: Option<String>,
    processor: Option<String>,
    clock_speed_ghz: Option<String>,
    gpu: Option<f64>,
    gpu_model: Option<String>,
    gpu_memory_gib: Option<f64>,
    fpga: Option<f64>,
    ebs_optimized: Option<bool>,
    ebs_baseline_bandwidth_mbps: Option<f64>,
    ebs_baseline_iops: Option<u64>,
    ebs_max_bandwidth_mbps: Option<f64>,
    ebs_max_iops: Option<u64>,
    enhanced_networking: Option<bool>,
    vpc_max_enis: Option<u32>,
    vpc_ips_per_eni: Option<u32>,
    storage_devices: Option<u32>,
    storage_size_gb: Option<f64>,
    storage_ssd: Option<bool>,
    storage_nvme: Option<bool>,
    #[serde(default)]
    linux_on_demand: BTreeMap<String, f64>,
}

fn matches_type_filters(instance: &Ec2TypeRecord, args: &Ec2TypesArgs) -> bool {
    if let Some(query) = &args.query {
        let query = query.to_lowercase();
        if !contains_casefold(&instance.instance_type, &query)
            && !optional_contains_casefold(instance.family.as_deref(), &query)
            && !optional_contains_casefold(instance.pretty_name.as_deref(), &query)
            && !optional_contains_casefold(instance.processor.as_deref(), &query)
        {
            return false;
        }
    }

    if let Some(arch) = &args.arch {
        let arch = arch.to_lowercase();
        if !instance
            .arch
            .iter()
            .any(|candidate| candidate.to_lowercase() == arch)
        {
            return false;
        }
    }

    if let Some(family) = &args.family {
        let family = family.to_lowercase();
        if !optional_contains_casefold(instance.family.as_deref(), &family) {
            return false;
        }
    }

    if args.current && instance.generation.as_deref() != Some("current") {
        return false;
    }

    if args.gpu && instance.gpu.unwrap_or_default() <= 0.0 {
        return false;
    }

    if let Some(min) = args.min_vcpu
        && instance.vcpu.unwrap_or_default() < min
    {
        return false;
    }

    if let Some(max) = args.max_vcpu
        && instance.vcpu.unwrap_or(u32::MAX) > max
    {
        return false;
    }

    if let Some(min) = args.min_memory
        && instance.memory_gib.unwrap_or_default() < min
    {
        return false;
    }

    if let Some(max) = args.max_memory
        && instance.memory_gib.unwrap_or(f64::MAX) > max
    {
        return false;
    }

    true
}

fn contains_casefold(value: &str, query: &str) -> bool {
    value.to_lowercase().contains(query)
}

fn optional_contains_casefold(value: Option<&str>, query: &str) -> bool {
    value.is_some_and(|value| contains_casefold(value, query))
}

fn sort_types(rows: &mut [&Ec2TypeRecord], sort: Ec2TypesSort, price_region: &str) {
    match sort {
        Ec2TypesSort::Type => {
            rows.sort_by(|left, right| left.instance_type.cmp(&right.instance_type))
        }
        Ec2TypesSort::Vcpu => rows.sort_by(|left, right| {
            left.vcpu
                .unwrap_or_default()
                .cmp(&right.vcpu.unwrap_or_default())
                .then_with(|| left.instance_type.cmp(&right.instance_type))
        }),
        Ec2TypesSort::Memory => rows.sort_by(|left, right| {
            compare_f64(
                left.memory_gib.unwrap_or_default(),
                right.memory_gib.unwrap_or_default(),
            )
            .then_with(|| left.instance_type.cmp(&right.instance_type))
        }),
        Ec2TypesSort::Price => rows.sort_by(|left, right| {
            compare_optional_f64(
                left.linux_on_demand.get(price_region).copied(),
                right.linux_on_demand.get(price_region).copied(),
            )
            .then_with(|| left.instance_type.cmp(&right.instance_type))
        }),
    }
}

fn compare_f64(left: f64, right: f64) -> Ordering {
    left.partial_cmp(&right).unwrap_or(Ordering::Equal)
}

fn compare_optional_f64(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => compare_f64(left, right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn type_row(
    instance: &Ec2TypeRecord,
    args: &Ec2TypesArgs,
    price_region: &str,
) -> serde_json::Value {
    let mut row = serde_json::Map::new();
    row.insert(
        "instance_type".to_owned(),
        serde_json::Value::String(instance.instance_type.clone()),
    );
    insert_optional_string(&mut row, "family", instance.family.as_deref());
    insert_optional_u32(&mut row, "vcpu", instance.vcpu);
    insert_optional_f64(&mut row, "memory_gib", instance.memory_gib);
    row.insert(
        "arch".to_owned(),
        serde_json::Value::String(instance.arch.join(",")),
    );
    insert_optional_string(&mut row, "generation", instance.generation.as_deref());
    insert_optional_string(&mut row, "network", instance.network_performance.as_deref());
    insert_optional_f64(&mut row, "gpu", instance.gpu);
    row.insert(
        "storage".to_owned(),
        serde_json::Value::String(storage_summary(instance)),
    );

    if args.wide {
        insert_optional_string(&mut row, "pretty_name", instance.pretty_name.as_deref());
        insert_optional_string(&mut row, "processor", instance.processor.as_deref());
        insert_optional_string(
            &mut row,
            "clock_speed_ghz",
            instance.clock_speed_ghz.as_deref(),
        );
        insert_optional_string(&mut row, "gpu_model", instance.gpu_model.as_deref());
        insert_optional_f64(&mut row, "gpu_memory_gib", instance.gpu_memory_gib);
        insert_optional_f64(&mut row, "fpga", instance.fpga);
        insert_optional_bool(&mut row, "ebs_optimized", instance.ebs_optimized);
        insert_optional_f64(
            &mut row,
            "ebs_baseline_bandwidth_mbps",
            instance.ebs_baseline_bandwidth_mbps,
        );
        insert_optional_u64(&mut row, "ebs_baseline_iops", instance.ebs_baseline_iops);
        insert_optional_f64(
            &mut row,
            "ebs_max_bandwidth_mbps",
            instance.ebs_max_bandwidth_mbps,
        );
        insert_optional_u64(&mut row, "ebs_max_iops", instance.ebs_max_iops);
        insert_optional_bool(
            &mut row,
            "enhanced_networking",
            instance.enhanced_networking,
        );
        insert_optional_u32(&mut row, "vpc_max_enis", instance.vpc_max_enis);
        insert_optional_u32(&mut row, "vpc_ips_per_eni", instance.vpc_ips_per_eni);
    }

    if !args.no_price {
        row.insert(
            "price_region".to_owned(),
            serde_json::Value::String(price_region.to_owned()),
        );
        if let Some(price) = instance.linux_on_demand.get(price_region) {
            insert_optional_f64(&mut row, "linux_ondemand_usd_per_hour", Some(*price));
        }
    }

    if args.all_prices
        && let Ok(value) = serde_json::to_value(&instance.linux_on_demand)
    {
        row.insert("linux_on_demand".to_owned(), value);
    }

    serde_json::Value::Object(row)
}

fn storage_summary(instance: &Ec2TypeRecord) -> String {
    let Some(devices) = instance.storage_devices else {
        return "EBS only".to_owned();
    };
    let mut parts = Vec::new();
    parts.push(format!("{devices} x"));
    if let Some(size) = instance.storage_size_gb {
        parts.push(format_number(size));
        parts.push("GB".to_owned());
    } else {
        parts.push("unknown-size".to_owned());
    }
    if instance.storage_nvme == Some(true) {
        parts.push("NVMe".to_owned());
    }
    if instance.storage_ssd == Some(true) {
        parts.push("SSD".to_owned());
    }
    parts.join(" ")
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

fn price_region(globals: &Globals, args: &Ec2TypesArgs) -> String {
    args.price_region
        .clone()
        .or_else(|| globals.region.clone())
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .unwrap_or_else(|| "us-east-1".to_owned())
}

fn insert_optional_string(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        row.insert(key.to_owned(), serde_json::Value::String(value.to_owned()));
    }
}

fn insert_optional_bool(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<bool>,
) {
    if let Some(value) = value {
        row.insert(key.to_owned(), serde_json::Value::Bool(value));
    }
}

fn insert_optional_u32(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u32>,
) {
    if let Some(value) = value {
        row.insert(key.to_owned(), serde_json::Value::from(value));
    }
}

fn insert_optional_u64(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u64>,
) {
    if let Some(value) = value {
        row.insert(key.to_owned(), serde_json::Value::from(value));
    }
}

fn insert_optional_f64(
    row: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<f64>,
) {
    if let Some(value) = value {
        let json_value = if value.is_finite()
            && value.fract() == 0.0
            && value >= i64::MIN as f64
            && value <= i64::MAX as f64
        {
            serde_json::Value::from(value as i64)
        } else {
            serde_json::Value::from(value)
        };
        row.insert(key.to_owned(), json_value);
    }
}
