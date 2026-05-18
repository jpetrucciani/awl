use aws_config::BehaviorVersion;
use aws_config::default_provider::region::DefaultRegionChain;
use aws_config::environment::EnvironmentVariableRegionProvider;
use aws_config::meta::region::RegionProviderChain;
use aws_config::profile::ProfileFileRegionProvider;
use aws_config::sts::AssumeRoleProvider;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_types::region::Region;

use crate::cli::Globals;
use crate::error::Result;

#[derive(Clone)]
pub struct AwsContext {
    globals: Globals,
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
        if let Some(max_attempts) = globals.max_attempts {
            loader = loader.retry_config(
                aws_config::retry::RetryConfig::standard().with_max_attempts(max_attempts),
            );
        }

        let mut shared = loader.load().await;
        if let Some(role_arn) = &globals.role_arn {
            if globals.mfa_token.is_some() {
                return Err(crate::error::AwlError::Usage {
                    message: "global --mfa-token is only supported by `awl sts assume` for now"
                        .to_owned(),
                });
            }
            let session_name = globals
                .role_session_name
                .clone()
                .unwrap_or_else(|| format!("awl-{}", chrono::Utc::now().timestamp()));
            let provider = AssumeRoleProvider::builder(role_arn)
                .session_name(session_name)
                .configure(&shared)
                .build()
                .await;
            shared = shared
                .into_builder()
                .credentials_provider(SharedCredentialsProvider::new(provider))
                .build();
        }

        Ok(Self {
            globals: globals.clone(),
            shared,
        })
    }

    pub fn s3(&self, path_style: bool) -> aws_sdk_s3::Client {
        let mut builder = aws_sdk_s3::config::Builder::from(&self.shared);
        if let Some(endpoint) = self.endpoint_for("S3") {
            builder = builder.endpoint_url(endpoint);
        }
        if path_style {
            builder = builder.force_path_style(true);
        }
        aws_sdk_s3::Client::from_conf(builder.build())
    }

    pub fn sqs(&self) -> aws_sdk_sqs::Client {
        let mut builder = aws_sdk_sqs::config::Builder::from(&self.shared);
        if let Some(endpoint) = self.endpoint_for("SQS") {
            builder = builder.endpoint_url(endpoint);
        }
        aws_sdk_sqs::Client::from_conf(builder.build())
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

    fn endpoint_for(&self, service: &str) -> Option<String> {
        if let Some(endpoint) = &self.globals.endpoint_url {
            return Some(endpoint.clone());
        }

        let service_key = format!("AWS_ENDPOINT_URL_{service}");
        std::env::var(service_key)
            .ok()
            .or_else(|| std::env::var("AWS_ENDPOINT_URL").ok())
    }
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
