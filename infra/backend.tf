# Partial backend configuration, completed by backend.hcl:
#
#   tofu init -backend-config=backend.hcl
#
# use_lockfile in backend.hcl enables S3-native state locking (OpenTofu >= 1.10),
# which replaces the old DynamoDB lock table — one less resource to secure.
terraform {
  backend "s3" {}
}
