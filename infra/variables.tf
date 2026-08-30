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
