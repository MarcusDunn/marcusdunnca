variable "project" {
  description = "Short project slug, used as a prefix for resource names and tags."
  type        = string
  default     = "marcusdunnca"
}

variable "aws_region" {
  description = "Primary AWS region."
  type        = string
  default     = "ca-central-1"
}


variable "app_domain" {
  description = <<-EOT
    Fully-qualified domain the application is served from.

    marcusdunn.ca is registered at Cloudflare and stays there — no Route53
    delegation. A hosted zone would cost $0.50/month to automate two DNS records
    that are created once and then left alone.
  EOT
  type        = string
  default     = "study.aws.marcusdunn.ca"
}

variable "webauthn_rp_id" {
  description = <<-EOT
    WebAuthn Relying Party ID. Must be a registrable suffix of app_domain.

    Deliberately the apex, not the app subdomain: an RP ID cannot be widened
    later without re-registering every passkey, so starting at the apex keeps
    future subdomains usable with the same credentials.
  EOT
  type        = string
  default     = "marcusdunn.ca"
}

variable "dynamodb_read_capacity" {
  description = "Provisioned RCU. The perpetual free tier covers 25; on-demand mode is NOT covered and bills from the first request."
  type        = number
  default     = 5
}

variable "dynamodb_write_capacity" {
  description = "Provisioned WCU. See dynamodb_read_capacity."
  type        = number
  default     = 5
}

variable "webauthn_credentials" {
  description = <<-EOT
    Registered passkeys, as a JSON array of {id, public_key} objects.

    This is public data by construction — a WebAuthn credential ID and its
    public key are what the browser hands any relying party during a ceremony,
    and neither can be used to forge an assertion. It is a plain environment
    variable rather than an SSM parameter for that reason: nothing here is worth
    the KMS call on every cold start.

    Empty by default. There is no self-service registration endpoint; enrolling
    a passkey means pasting its record here and applying, which is the intended
    friction for a single-user app.
  EOT
  type        = string
  default     = "[]"
}

variable "bedrock_model_id" {
  description = <<-EOT
    Model the generate function invokes.

    The `ca.` prefix is an in-region inference profile, not a foundation-model
    ID: Nova Lite is one of the few models with a genuine Canadian profile, so
    document text never leaves ca-central-1. Switching to a `us.` or `global.`
    profile silently changes that, and would also need bedrock_allowed_models in
    bootstrap/ widened to match.
  EOT
  type        = string
  default     = "ca.amazon.nova-lite-v1:0"
}

variable "max_pages" {
  description = <<-EOT
    Pages of a document the generate function will read before giving up.

    Bedrock bills per input token and a PDF page is on the order of a thousand
    of them, so this is the per-document cost ceiling. It is enforced in the
    handler because IAM has no condition key for token count.
  EOT
  type        = number
  default     = 100
}

variable "daily_document_cap" {
  description = <<-EOT
    Documents the generate function will process in a rolling day.

    The second half of the cost ceiling: max_pages bounds one invocation, this
    bounds how many invocations a runaway upload loop can produce. Counted in
    DynamoDB by the handler — S3 notifications have no throttle of their own,
    and the account budget alarm fires hours after the money is gone.
  EOT
  type        = number
  default     = 20
}

variable "max_upload_bytes" {
  description = <<-EOT
    Largest PDF the presigned PUT will accept.

    Bound by Bedrock, not by S3. The Converse document block caps around 4.5 MB,
    so a larger file uploads perfectly and then fails generation — the worst
    shape of failure, because the cost is paid and the feedback arrives a minute
    later on a different screen. Rejecting at the create call fails it in the
    place the user is looking.
  EOT
  type        = number
  default     = 4500000
}


variable "registration_token" {
  description = <<-EOT
    Shared secret gating the one-shot passkey enrolment routes. Empty disables
    them entirely.

    THIS IS SECRET AND MUST NOT BE COMMITTED. Note .gitignore ignores *.tfvars
    but deliberately un-ignores *.auto.tfvars — so this must not go in an
    auto.tfvars file.

    Deliberately not wired to CI. Apply it locally for the ceremony
    (`tofu apply -var=registration_token=...`), and the next CI apply reverts it
    to empty, which closes the enrolment window automatically rather than
    depending on anyone remembering.

    Minimum 32 characters, enforced at cold start. Generate with
    `openssl rand -base64 24`. Between enabling this and pasting credentials,
    enrolment is protected only by this secret on an unauthenticated endpoint
    whose sole throttle is the account concurrency limit of 10 — keep the window
    to minutes.
  EOT
  type        = string
  sensitive   = true
  default     = ""
}
