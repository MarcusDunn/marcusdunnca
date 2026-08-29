# Partial backend configuration, completed by backend.hcl.
#
# Bootstrap has a chicken-and-egg problem: it creates the very bucket that holds
# its own state. So scripts/bootstrap.sh temporarily moves this file aside for
# the first apply (which runs against local state), generates backend.hcl from
# the resulting outputs, restores this file, and then migrates:
#
#   tofu init -migrate-state -backend-config=backend.hcl
#
# After that first run this is the only mode of operation, and backend.hcl is
# committed so every later init is reproducible:
#
#   tofu init -backend-config=backend.hcl
terraform {
  backend "s3" {}
}
