#!/usr/bin/env bash
#
# Makes the two Cloudflare API tokens that CI uses for infra/dns.tf, and writes
# them to SSM. Run by a human from a workstation, after scripts/bootstrap.sh
# has applied the IAM statements that read these parameters.
#
#   /ci/cloudflare/dns-read  DNS Read,  read by the plan role (pull requests)
#   /ci/cloudflare/dns-edit  DNS Write, read by the apply role (production)
#
# Both tokens are limited to the marcusdunn.ca zone and expire after one year.
# To rotate them, run this script again. It makes new tokens, writes them to
# SSM, and then deletes the old tokens that have the same names.
#
# You need a Cloudflare token with User > API Tokens > Edit only (the "Create
# Additional Tokens" template). Use it for this script, then delete it. That
# permission can make a token with any permission, so do not keep it.
#
# No token value goes to the terminal, to a file, or to a command line.
#
#   nix develop
#   aws sso login
#   ./scripts/cloudflare-setup.sh
set -euo pipefail
umask 077
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

REGION="${AWS_REGION:-ca-central-1}"
EXPECTED_ACCOUNT_ID="812642122818"
ZONE_NAME="marcusdunn.ca"
ZONE_ID="124e581f9ae18c786ba0eac9fd00aaac"
READ_NAME="marcusdunnca-ci-dns-read"
EDIT_NAME="marcusdunnca-ci-dns-edit"
APP_DOMAIN="study.aws.marcusdunn.ca"

echo "==> Verifying AWS identity"
ACCOUNT_ID="$(aws sts get-caller-identity --query Account --output text)"
if [ "$ACCOUNT_ID" != "$EXPECTED_ACCOUNT_ID" ]; then
  echo "Wrong AWS account: $ACCOUNT_ID (expected $EXPECTED_ACCOUNT_ID)." >&2
  exit 1
fi

read -rsp "Cloudflare token with API Tokens > Edit: " BOOTSTRAP_TOKEN
echo
export BOOTSTRAP_TOKEN

# The token goes in a header file on stdin (-H @-), not on the command line,
# so other processes cannot see it in the process list.
cf() {
  local method="$1" path="$2"
  shift 2
  printf 'Authorization: Bearer %s\n' "$BOOTSTRAP_TOKEN" |
    curl -fsS -X "$method" -H @- -H 'Content-Type: application/json' \
      "https://api.cloudflare.com/client/v4$path" "$@"
}

echo "==> Finding permission groups"
GROUPS_JSON="$(cf GET /user/tokens/permission_groups)"
group_id() {
  local id
  id="$(jq -r --arg n "$1" '
    [.result[]
     | select(.name == $n)
     | select(any(.scopes[]; . == "com.cloudflare.api.account.zone"))
     | .id] as $ids
    | if ($ids | length) == 1 then $ids[0] else empty end' <<<"$GROUPS_JSON")"
  if [ -z "$id" ]; then
    echo "Did not find exactly one zone permission group named '$1'." >&2
    exit 1
  fi
  echo "$id"
}
DNS_READ_ID="$(group_id "DNS Read")"
DNS_WRITE_ID="$(group_id "DNS Write")"

EXPIRES="$(date -u -d '+1 year' +%Y-%m-%dT%H:%M:%SZ 2>/dev/null ||
  date -u -v+1y +%Y-%m-%dT%H:%M:%SZ)"

OLD_TOKENS_JSON="$(cf GET '/user/tokens?per_page=50')"

# Makes a token and writes its value to SSM. The value stays in a variable and
# goes to the AWS CLI as JSON on stdin.
make_token() {
  local name="$1" group="$2" parameter="$3" body
  body="$(jq -n --arg name "$name" --arg group "$group" --arg zone "$ZONE_ID" --arg expires "$EXPIRES" '{
    name: $name,
    expires_on: $expires,
    policies: [{
      effect: "allow",
      resources: {("com.cloudflare.api.account.zone." + $zone): "*"},
      permission_groups: [{id: $group}]
    }]
  }')"

  echo "==> Making $name (expires $EXPIRES)"
  TOKEN_VALUE="$(cf POST /user/tokens --data "$body" | jq -er .result.value)"
  export TOKEN_VALUE

  # AWS CLI v2 cannot read file:///dev/stdin from a pipe, and --value would
  # put the token on the command line. So the JSON goes in a file that only
  # this user can read, and the EXIT trap deletes it.
  echo "==> Writing $parameter"
  jq -n --arg name "$parameter" '{
    Name: $name,
    Type: "SecureString",
    Overwrite: true,
    Description: "Cloudflare API token for infra/dns.tf. Written by scripts/cloudflare-setup.sh.",
    Value: env.TOKEN_VALUE
  }' >"$WORK_DIR/parameter.json"
  aws ssm put-parameter --region "$REGION" --cli-input-json "file://$WORK_DIR/parameter.json" >/dev/null
  rm -f "$WORK_DIR/parameter.json"
}

make_token "$READ_NAME" "$DNS_READ_ID" /ci/cloudflare/dns-read
READ_TOKEN="$TOKEN_VALUE"
make_token "$EDIT_NAME" "$DNS_WRITE_ID" /ci/cloudflare/dns-edit
unset TOKEN_VALUE

# Delete tokens from an earlier run, only after both new values are in SSM.
for name in "$READ_NAME" "$EDIT_NAME"; do
  for id in $(jq -r --arg n "$name" '.result[] | select(.name == $n) | .id' <<<"$OLD_TOKENS_JSON"); do
    echo "==> Deleting old token $name ($id)"
    cf DELETE "/user/tokens/$id" >/dev/null
  done
done

# A new token can take a few seconds to work.
sleep 5

echo
echo "==> Record IDs for the import blocks in infra/dns.tf"
printf 'Authorization: Bearer %s\n' "$READ_TOKEN" |
  curl -fsS -H @- "https://api.cloudflare.com/client/v4/zones/$ZONE_ID/dns_records?per_page=100" |
  jq -r --arg app "$APP_DOMAIN" '
    .result[]
    | select(.name == $app or (.name | endswith("." + $app)) or .name == "vpn.marcusdunn.ca")
    | "\(.id)  \(.type)  \(.name) -> \(.content)  proxied=\(.proxied)"'
unset READ_TOKEN BOOTSTRAP_TOKEN

echo
echo "Done. Now delete the token with API Tokens > Edit in the Cloudflare dashboard."
echo "Zone: $ZONE_NAME ($ZONE_ID)"
