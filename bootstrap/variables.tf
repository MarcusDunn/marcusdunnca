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
