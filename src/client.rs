use aws_config::BehaviorVersion;
use aws_config::default_provider::region::DefaultRegionChain;
use aws_config::environment::EnvironmentVariableRegionProvider;
use aws_config::meta::region::RegionProviderChain;
use aws_config::profile::ProfileFileRegionProvider;
use aws_config::retry::RetryConfig;
use aws_config::sts::AssumeRoleProvider;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_types::region::Region;

use crate::cli::{Globals, RetryMode};
use crate::error::{AwlError, Result};

#[derive(Clone)]
pub struct AwsContext {
    shared: aws_config::SdkConfig,
}

impl AwsContext {
    pub async fn new(globals: &Globals) -> Result<Self> {
        Self::new_with_region_provider(globals, default_region_provider(globals)).await
    }

    pub async fn new_without_imds_region(globals: &Globals) -> Result<Self> {
        Self::new_with_region_provider(globals, non_imds_region_provider(globals)).await
    }

    async fn new_with_region_provider(
        globals: &Globals,
        region_provider: RegionProviderChain,
    ) -> Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest()).region(region_provider);
        if let Some(profile) = &globals.profile {
            loader = loader.profile_name(profile);
        }
        // Only the CLI flag is applied here; the SDK already resolves
        // AWS_ENDPOINT_URL_<SERVICE> before AWS_ENDPOINT_URL.
        if let Some(endpoint) = &globals.endpoint_url {
            loader = loader.endpoint_url(endpoint);
        }
        if globals.retry_mode.is_some() || globals.max_attempts.is_some() {
            let mut retry = match globals.retry_mode {
                Some(RetryMode::Adaptive) => RetryConfig::adaptive(),
                Some(RetryMode::Standard | RetryMode::Legacy) | None => RetryConfig::standard(),
            };
            if let Some(max_attempts) = globals.max_attempts {
                retry = retry.with_max_attempts(max_attempts);
            }
            loader = loader.retry_config(retry);
        }

        let mut shared = loader.load().await;
        if let Some(role_arn) = &globals.role_arn {
            if globals.mfa_token.is_some() {
                return Err(AwlError::Usage {
                    message: "global --mfa-token is only supported by `awl sts assume` for now"
                        .to_owned(),
                });
            }
            let provider = AssumeRoleProvider::builder(role_arn)
                .session_name(session_name(globals))
                .configure(&shared)
                .build()
                .await;
            shared = shared
                .into_builder()
                .credentials_provider(SharedCredentialsProvider::new(provider))
                .build();
        }

        Ok(Self { shared })
    }

    pub fn region(&self) -> Option<&str> {
        self.shared.region().map(Region::as_ref)
    }

    pub fn endpoint_url(&self) -> Option<&str> {
        self.shared.endpoint_url()
    }

    pub fn s3(&self, path_style: bool, region: Option<Region>) -> aws_sdk_s3::Client {
        let mut config =
            aws_sdk_s3::config::Builder::from(&self.shared).force_path_style(path_style);
        if let Some(region) = region {
            config = config.region(region);
        }
        aws_sdk_s3::Client::from_conf(config.build())
    }

    pub fn sqs(&self) -> aws_sdk_sqs::Client {
        aws_sdk_sqs::Client::new(&self.shared)
    }

    pub fn sts(&self) -> aws_sdk_sts::Client {
        aws_sdk_sts::Client::new(&self.shared)
    }

    pub fn ecr(&self) -> aws_sdk_ecr::Client {
        aws_sdk_ecr::Client::new(&self.shared)
    }

    pub fn ssm(&self) -> aws_sdk_ssm::Client {
        aws_sdk_ssm::Client::new(&self.shared)
    }

    pub fn secrets(&self) -> aws_sdk_secretsmanager::Client {
        aws_sdk_secretsmanager::Client::new(&self.shared)
    }

    pub fn logs(&self) -> aws_sdk_cloudwatchlogs::Client {
        aws_sdk_cloudwatchlogs::Client::new(&self.shared)
    }

    pub fn ec2(&self) -> aws_sdk_ec2::Client {
        aws_sdk_ec2::Client::new(&self.shared)
    }

    pub fn lambda(&self) -> aws_sdk_lambda::Client {
        aws_sdk_lambda::Client::new(&self.shared)
    }

    pub fn ddb(&self) -> aws_sdk_dynamodb::Client {
        aws_sdk_dynamodb::Client::new(&self.shared)
    }

    pub fn route53(&self) -> aws_sdk_route53::Client {
        aws_sdk_route53::Client::new(&self.shared)
    }

    pub fn kms(&self) -> aws_sdk_kms::Client {
        aws_sdk_kms::Client::new(&self.shared)
    }
}

pub fn session_name(globals: &Globals) -> String {
    globals
        .role_session_name
        .clone()
        .unwrap_or_else(|| format!("awl-{}", chrono::Utc::now().timestamp()))
}

fn default_region_provider(globals: &Globals) -> RegionProviderChain {
    let cli_region = globals
        .region
        .as_ref()
        .map(|region| Region::new(region.clone()));
    let mut default_region = DefaultRegionChain::builder();
    if let Some(profile) = &globals.profile {
        default_region = default_region.profile_name(profile);
    }

    RegionProviderChain::first_try(cli_region)
        .or_else(default_region.build())
        .or_else(Region::new("us-east-1"))
}

fn non_imds_region_provider(globals: &Globals) -> RegionProviderChain {
    let cli_region = globals
        .region
        .as_ref()
        .map(|region| Region::new(region.clone()));
    let profile_region = match &globals.profile {
        Some(profile) => ProfileFileRegionProvider::builder()
            .profile_name(profile)
            .build(),
        None => ProfileFileRegionProvider::new(),
    };

    RegionProviderChain::first_try(cli_region)
        .or_else(EnvironmentVariableRegionProvider::new())
        .or_else(profile_region)
        .or_else(Region::new("us-east-1"))
}
