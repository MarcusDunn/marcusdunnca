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

variable "github_owner" {
  description = "GitHub user or org that owns the infrastructure repo."
  type        = string
  default     = "MarcusDunn"
}

variable "github_repo" {
  description = "GitHub repository permitted to assume the CI roles."
  type        = string
  default     = "marcusdunnca"
}

variable "github_owner_id" {
  description = <<-EOT
    Numeric GitHub account ID of the owner.

    GitHub issues OIDC subjects in immutable form —
    `repo:OWNER@<owner_id>/REPO@<repo_id>:...` — and the repository is pinned to
    that format via the actions/oidc/customization/sub API. Binding trust to the
    numeric IDs rather than the names means renaming the repo, or a third party
    later claiming the freed-up name `MarcusDunn/marcusdunnca`, cannot produce a
    token this account will accept.
  EOT
  type        = number
  default     = 51931484
}

variable "github_repo_id" {
  description = "Numeric GitHub repository ID. See github_owner_id for why IDs are used."
  type        = number
  default     = 1350868756
}

variable "allowed_regions" {
  description = <<-EOT
    Regions the CI roles may operate in. Everything else is denied outright,
    which is the single most effective control against denial-of-wallet: the
    usual move after stealing credentials is to spin up compute in every region
    at once, and this makes all but one of those calls fail.

    us-east-1 is included because several global services (IAM, CloudTrail's
    global endpoint, account contacts) are only addressable there.
  EOT
  type        = list(string)
  default     = ["ca-central-1", "us-east-1"]
}

variable "apply_environment" {
  description = <<-EOT
    Name of the GitHub Environment that gates apply. The apply role's trust policy
    accepts ONLY tokens carrying this environment in their `sub` claim, so apply is
    impossible from any job that does not declare `environment: <this value>`.
  EOT
  type        = string
  default     = "production"
}

variable "state_bucket_use_cmk" {
  description = <<-EOT
    Encrypt the state bucket with a customer-managed KMS key instead of SSE-S3.

    A CMK gives per-key access control, key-level CloudTrail data events, and lets
    you revoke access to state independently of S3 permissions. It costs ~$1/month
    for the key plus per-request charges, which is why it defaults to off given the
    free-tier-only baseline. Flip to true if state ever holds real secrets.
  EOT
  type        = bool
  default     = false
}

variable "state_noncurrent_version_retention_days" {
  description = "How long to retain superseded state versions before expiring them."
  type        = number
  default     = 90
}

variable "cloudtrail_retention_days" {
  description = "How long to keep CloudTrail logs in S3 before expiring them."
  type        = number
  default     = 400
}

variable "security_contact" {
  description = <<-EOT
    Alternate SECURITY contact for the account. AWS uses this to reach a human
    about abuse reports and compromised-credential notices rather than falling
    back to the root mailbox.

    All four fields are required by the API, so leave this null to skip
    registering the contact entirely.
  EOT
  type = object({
    name          = string
    title         = string
    email_address = string
    phone_number  = string
  })
  default = null
}

variable "account_alias" {
  description = "IAM account alias, which also becomes the friendly console sign-in URL."
  type        = string
  default     = "marcusdunnca"
}

variable "monthly_budget_usd" {
  description = <<-EOT
    Monthly cost budget in USD. Alerts fire at 80% actual, 100% actual, and on a
    forecast breach of 100%.

    $10 covers the Reading Trainer workload: Bedrock at roughly 15c/document
    plus a few cents of S3 and CloudTrail data events. Raised from $1 when
    Bedrock was allowlisted — at 15c/doc the old threshold would have fired
    after about seven documents, which is normal use, not an anomaly.
  EOT
  type        = string
  default     = "10"
}

variable "cost_alert_emails" {
  description = <<-EOT
    Where budget and cost-anomaly alerts go. More than one on purpose: the
    marcusdunn.ca alias is Cloudflare-forwarded, and an alerting channel that
    depends on a forwarding rule you cannot monitor will fail silently exactly
    when it matters.

    Each address gets a one-time SNS confirmation email; alerts do not flow to
    an address until its link is clicked.
  EOT
  type        = list(string)
  default     = ["aws-root@marcusdunn.ca", "marcus.s.dunn@gmail.com"]
}

variable "bedrock_allowed_model_families" {
  description = <<-EOT
    Claude model families the application may invoke, as ARN fragments.

    There is no IAM condition key for token count, so model choice is the only
    cost lever IAM offers. Opus costs several times Sonnet per token; omitting
    it caps the per-document worst case. Add a family here deliberately.
  EOT
  type        = list(string)
  default     = ["sonnet", "haiku"]
}
