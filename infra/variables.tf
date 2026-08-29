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

variable "account_alias" {
  description = "IAM account alias, which also becomes the friendly console sign-in URL."
  type        = string
  default     = "marcusdunnca"
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
