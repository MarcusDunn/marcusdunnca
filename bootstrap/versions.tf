terraform {
  # S3 native state locking (use_lockfile) requires OpenTofu >= 1.10.
  # That is what lets us drop the legacy DynamoDB lock table entirely.
  required_version = ">= 1.10.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }
    tls = {
      source  = "hashicorp/tls"
      version = "~> 4.0"
    }
  }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = {
      Project   = var.project
      ManagedBy = "opentofu"
      Module    = "bootstrap"
    }
  }
}
