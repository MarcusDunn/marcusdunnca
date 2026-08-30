terraform {
  required_version = ">= 1.10.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
    }

    # Only ever used to build the placeholder Lambda package. Real code is
    # shipped by the build workflow, not from here — see infra/lambda.tf.
    archive = {
      source  = "hashicorp/archive"
      version = "~> 2.7"
    }
  }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = {
      Project   = var.project
      ManagedBy = "opentofu"
      Module    = "infra"
    }
  }
}

# CloudFront accepts certificates from us-east-1 and nowhere else, regardless of
# where the distribution or its origin live.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = {
      Project   = var.project
      ManagedBy = "opentofu"
      Module    = "infra"
    }
  }
}
