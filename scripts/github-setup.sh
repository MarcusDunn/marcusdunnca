#!/usr/bin/env bash
#
# Configures the GitHub side: repo settings, the protected `production`
# environment, the main-branch ruleset, and the CI role ARN variables.
#
# GitHub is configured with `gh` rather than the Terraform github provider on
# purpose: that provider needs a long-lived PAT, which is exactly the kind of
# credential this setup exists to avoid. This script is the reproducible
# artifact instead.
#
# Idempotent — safe to re-run after changing anything below.
set -euo pipefail

cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

OWNER="${GITHUB_OWNER:-MarcusDunn}"
REPO="${GITHUB_REPO:-marcusdunnca}"
SLUG="${OWNER}/${REPO}"
ENVIRONMENT="production"

echo "==> Target repository: $SLUG"

# ---------------------------------------------------------------------------
# Repository settings
# ---------------------------------------------------------------------------
echo "==> Applying repository settings"
gh api -X PATCH "repos/${SLUG}" \
  -F allow_auto_merge=true \
  -F delete_branch_on_merge=true \
  -F allow_squash_merge=true \
  -F allow_merge_commit=false \
  -F allow_rebase_merge=false \
  -F has_wiki=false \
  -F has_projects=false \
  -F web_commit_signoff_required=true \
  >/dev/null
echo "    Squash-only merges (keeps history linear), auto-merge enabled."

# Free on public repositories. Push protection is the useful half: it rejects a
# commit containing a recognised credential at push time, rather than telling you
# about it after it is already in a public history and must be treated as burned.
echo "==> Enabling secret scanning and push protection"
gh api -X PATCH "repos/${SLUG}" --input - >/dev/null <<'JSON'
{
  "security_and_analysis": {
    "secret_scanning": { "status": "enabled" },
    "secret_scanning_push_protection": { "status": "enabled" }
  }
}
JSON

# ---------------------------------------------------------------------------
# Who and what is allowed to run Actions.
#
# This repo is public, so any stranger can fork it and open a PR. Workflow runs
# on such PRs must be approved by a maintainer first — not merely for first-time
# contributors (GitHub's default), but for every external contributor, forever.
# ---------------------------------------------------------------------------
echo "==> Requiring approval for all external contributors' workflow runs"
gh api -X PUT "repos/${SLUG}/actions/permissions/fork-pr-contributor-approval" \
  -f approval_policy=all_external_contributors >/dev/null

# An allowlist of exactly the third-party actions this repo uses. A compromised
# action runs in a job holding an OIDC token, so "any action from anywhere" is
# not an acceptable default. actions/* is covered by github_owned_allowed.
#
# sha_pinning_required makes GitHub reject a workflow referencing an action by
# tag rather than commit SHA — enforcing at the platform level what the
# workflows already do by convention, so it cannot regress in review.
echo "==> Restricting allowed actions and enforcing SHA pinning"
gh api -X PUT "repos/${SLUG}/actions/permissions" --input - >/dev/null <<'JSON'
{ "enabled": true, "allowed_actions": "selected", "sha_pinning_required": true }
JSON

gh api -X PUT "repos/${SLUG}/actions/permissions/selected-actions" --input - >/dev/null <<'JSON'
{
  "github_owned_allowed": true,
  "verified_allowed": false,
  "patterns_allowed": [
    "aws-actions/configure-aws-credentials@*",
    "opentofu/setup-opentofu@*",
    "dependabot/fetch-metadata@*",
    "DeterminateSystems/nix-installer-action@*"
  ]
}
JSON

# Workflows get read-only tokens unless they ask for more in their `permissions`
# block. Every workflow in this repo declares exactly what it needs.
echo "==> Restricting default workflow token to read-only"
gh api -X PUT "repos/${SLUG}/actions/permissions/workflow" \
  -F default_workflow_permissions=read \
  -F can_approve_pull_request_reviews=true \
  >/dev/null
echo "    Note: can_approve_pull_request_reviews must stay true so the"
echo "    lock-refresh workflow can open PRs. It grants nothing here because"
echo "    the ruleset requires 0 approvals, so a bot approval is worthless."

# ---------------------------------------------------------------------------
# CI role ARNs, read from bootstrap outputs
# ---------------------------------------------------------------------------
if [ -f "${REPO_ROOT}/bootstrap/backend.hcl" ]; then
  echo "==> Publishing CI role ARNs as repository variables"
  pushd "${REPO_ROOT}/bootstrap" >/dev/null
  PLAN_ROLE_ARN="$(tofu output -raw plan_role_arn)"
  APPLY_ROLE_ARN="$(tofu output -raw apply_role_arn)"
  popd >/dev/null

  # Variables, not secrets: these are ARNs, not credentials. Nothing can be done
  # with them without a matching OIDC token, and keeping them visible makes
  # workflow logs readable.
  gh variable set AWS_PLAN_ROLE_ARN  --repo "$SLUG" --body "$PLAN_ROLE_ARN"
  gh variable set AWS_APPLY_ROLE_ARN --repo "$SLUG" --body "$APPLY_ROLE_ARN"
  echo "    AWS_PLAN_ROLE_ARN  = $PLAN_ROLE_ARN"
  echo "    AWS_APPLY_ROLE_ARN = $APPLY_ROLE_ARN"
else
  echo "==> Skipping role variables (run scripts/bootstrap.sh first)"
fi

# ---------------------------------------------------------------------------
# Protected environment.
#
# This is half of the apply gate. Declaring `environment: production` in the
# workflow is what puts `environment:production` into the OIDC token subject,
# which the apply role's trust policy demands; restricting the environment to
# main is what stops any other branch from doing so.
# ---------------------------------------------------------------------------
echo "==> Creating protected '${ENVIRONMENT}' environment"
gh api -X PUT "repos/${SLUG}/environments/${ENVIRONMENT}" --input - >/dev/null <<'JSON'
{
  "deployment_branch_policy": {
    "protected_branches": false,
    "custom_branch_policies": true
  }
}
JSON

# An explicit `main` policy rather than "protected branches", because the latter
# has murky semantics now that protection comes from rulesets rather than
# classic branch protection.
if ! gh api "repos/${SLUG}/environments/${ENVIRONMENT}/deployment-branch-policies" \
     --jq '.branch_policies[].name' 2>/dev/null | grep -qx main; then
  gh api -X POST "repos/${SLUG}/environments/${ENVIRONMENT}/deployment-branch-policies" \
    -f name=main -f type=branch >/dev/null
fi
echo "    '${ENVIRONMENT}' deployable only from main."

# ---------------------------------------------------------------------------
# Main-branch ruleset
# ---------------------------------------------------------------------------
echo "==> Applying main-branch ruleset"

RULESET_JSON="$(cat <<'JSON'
{
  "name": "main",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] }
  },
  "bypass_actors": [],
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    { "type": "required_linear_history" },
    { "type": "required_signatures" },
    {
      "type": "pull_request",
      "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": true,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": true,
        "allowed_merge_methods": ["squash"]
      }
    },
    {
      "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": true,
        "required_status_checks": [
          { "context": "plan (bootstrap)" },
          { "context": "plan (infra)" }
        ]
      }
    }
  ]
}
JSON
)"

# Rulesets on a private repo need GitHub Pro. Detect that specifically rather
# than letting the error JSON get interpolated into the next request URL.
if ! RULESETS="$(gh api "repos/${SLUG}/rulesets" 2>&1)"; then
  if echo "$RULESETS" | grep -q "Upgrade to GitHub Pro"; then
    cat >&2 <<EOF

    UNABLE TO APPLY BRANCH PROTECTION.

    GitHub does not offer rulesets or branch protection on private
    repositories for accounts on the Free plan.

    Until this is resolved, main is UNPROTECTED: no required PR, no signed
    commit enforcement, no required status checks, and force-push is allowed.
    The OIDC apply gate still holds — the 'production' environment is
    restricted to main and the apply role trusts nothing else — but nothing
    stops a direct push to main from reaching it.

    Two ways forward:
      * GitHub Pro (~\$4/month), keeping the repo private
      * make the repo public, where rulesets are free

EOF
    exit 3
  fi
  echo "$RULESETS" >&2
  exit 1
fi

EXISTING_ID="$(echo "$RULESETS" | jq -r '.[] | select(.name=="main") | .id' | head -1)"
# Guard against a non-numeric id ever being spliced into the URL below.
case "$EXISTING_ID" in
  '' | *[!0-9]*) EXISTING_ID="" ;;
esac

if [ -n "$EXISTING_ID" ]; then
  echo "$RULESET_JSON" | gh api -X PUT "repos/${SLUG}/rulesets/${EXISTING_ID}" --input - >/dev/null
  echo "    Updated existing ruleset ($EXISTING_ID)."
else
  echo "$RULESET_JSON" | gh api -X POST "repos/${SLUG}/rulesets" --input - >/dev/null
  echo "    Created ruleset."
fi

cat <<EOF

==> GitHub configuration complete.

    bypass_actors is empty — the rules below apply to you too, with no override:

      * every change to main goes through a pull request
      * all commits must be signed and verified
      * linear history; squash merges only
      * force-push and branch deletion blocked
      * 'plan (bootstrap)' and 'plan (infra)' must pass, against current main

    Required checks only appear in the ruleset UI after they have reported once,
    so the very first PR is what makes them real.
EOF
