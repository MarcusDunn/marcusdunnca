//! Signed HTTP to Claude's Messages API on Amazon Bedrock.
//!
//! # Why this exists rather than an AWS SDK client
//!
//! This replaced `aws_sdk_bedrockruntime`'s Converse API. The models this app
//! wants are served by Bedrock's Messages-API endpoint
//! (`bedrock-mantle.{region}.api.aws`), which speaks Anthropic's native request
//! shape rather than Converse's normalised one. There is no generated AWS SDK
//! crate for it and no Anthropic SDK for Rust, so the request is built as JSON,
//! signed with SigV4, and posted directly.
//!
//! Two things are gained by moving off Converse. The models are reachable at all
//! — Converse's model table stops at Sonnet 4.6. And PDFs no longer need
//! citations switched on to be read visually: Converse forces that coupling,
//! this endpoint does not, which matters because `figure_recall` questions are
//! about what a page *looks* like.
//!
//! One thing is lost: Converse validated the request shape locally, in types.
//! Here a malformed body is a 400 from the service, so the shapes in
//! [`crate::bedrock`] are the only guard.
//!
//! # Why the endpoint region is configuration
//!
//! **There is no `bedrock-mantle` endpoint in Canada.** It resolves in US and EU
//! regions only — `ca-central-1` and `ca-west-1` have no DNS for it. The
//! previous integration called a ca-central-1 endpoint and used a `us.`
//! inference profile, so routing left Canada but the call did not. Here the
//! call itself leaves, and the region it leaves to is the one thing about that
//! worth making explicit rather than hardcoding.
//!
//! This is also why the permissions boundary's region lock needed a different
//! exemption than it had: the Lambda is now the caller into another region, not
//! a ca-central-1 caller whose routing targets happen to be elsewhere. See
//! `global_service_actions` in bootstrap/iam.tf.

use aws_credential_types::provider::ProvideCredentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use std::time::SystemTime;
use trainer_core::error::{aws, Error, Result};

/// SigV4 service name. Not `bedrock` — this endpoint signs as its own service,
/// and the IAM action is `bedrock-mantle:CreateInference`.
const SERVICE: &str = "bedrock-mantle";

/// The API version header the Messages API requires. Unrelated to the model, and
/// unchanged since 2023 — pinned rather than configurable because a different
/// value changes response shapes this crate parses.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Holds everything reused across invocations: the TLS pool and the credential
/// provider's cache. Built once at cold start and leaked, like the AWS clients
/// it sits beside.
pub struct Client {
    http: reqwest::Client,
    credentials: SharedCredentialsProvider,
    region: String,
    url: String,
}

impl Client {
    /// `region` is where the request is *sent*, which is not this function's
    /// region. Lambda runs in ca-central-1; this endpoint does not exist there.
    pub fn new(sdk: &aws_config::SdkConfig, region: String) -> Result<Self> {
        let credentials = sdk
            .credentials_provider()
            .ok_or_else(|| Error::Config("no AWS credentials provider available".into()))?;

        let http = reqwest::Client::builder()
            // A generation is a long call: a hundred-page PDF with thinking on
            // runs well past any default. Lambda's own timeout is the real
            // ceiling; this exists so a hung socket surfaces as an error rather
            // than consuming the whole invocation.
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(aws)?;

        Ok(Self {
            http,
            credentials,
            url: format!("https://{SERVICE}.{region}.api.aws/anthropic/v1/messages"),
            region,
        })
    }

    /// POST one Messages API request and hand back the parsed response body.
    ///
    /// Errors carry the service's message. The Messages API returns failures as
    /// a JSON envelope with an `error.message`, which is far more useful than
    /// the status line — a rejected `thinking` field or an unavailable model both
    /// arrive as a 400 or 403 whose body names the actual problem.
    pub async fn messages(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let bytes = serde_json::to_vec(body)?;

        let credentials = self.credentials.provide_credentials().await.map_err(aws)?;
        let identity = credentials.into();

        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name(SERVICE)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|e| Error::Aws(format!("building signing params: {e}")))?
            .into();

        // Every header signed here must also be sent, byte for byte, or the
        // signature does not verify. That is why they are listed once and reused
        // for both.
        let headers = [
            ("content-type", "application/json"),
            ("anthropic-version", ANTHROPIC_VERSION),
        ];

        let signable = SignableRequest::new(
            "POST",
            &self.url,
            headers.iter().copied(),
            SignableBody::Bytes(&bytes),
        )
        .map_err(|e| Error::Aws(format!("building signable request: {e}")))?;

        let (instructions, _signature) = sign(signable, &params)
            .map_err(|e| Error::Aws(format!("signing request: {e}")))?
            .into_parts();

        let mut request = self.http.post(&self.url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        // Authorization, x-amz-date, and x-amz-security-token for temporary
        // credentials. Added by the signer rather than by hand precisely so the
        // session-token case cannot be forgotten.
        for header in instructions.headers() {
            request = request.header(header.0, header.1);
        }

        let response = request.body(bytes).send().await.map_err(aws)?;
        let status = response.status();
        let text = response.text().await.map_err(aws)?;

        if !status.is_success() {
            let detail = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| text.chars().take(200).collect());

            // 400 is the model rejecting the request and will reject it again on
            // retry; 403 is model access or IAM. Neither is worth a Lambda
            // retry, but both are infrastructure rather than the document's
            // fault, so they stay `Aws` and surface in CloudWatch.
            return Err(Error::Aws(format!("bedrock {status}: {detail}")));
        }

        Ok(serde_json::from_str(&text)?)
    }
}
