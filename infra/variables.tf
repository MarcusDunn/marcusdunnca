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
