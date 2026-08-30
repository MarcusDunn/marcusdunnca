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

# Fail loudly and early. `tofu` is only needed midway through, to read the role
# ARNs from bootstrap outputs — without this check the script aborts there under
# `set -e`, having already applied half the settings, and the later steps
# (the environment and the ruleset) are silently skipped.
for cmd in gh tofu jq; do
  command -v "$cmd" >/dev/null || {
    echo "Missing '$cmd'. Run this from inside 'nix develop'." >&2
    exit 1
  }
done

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
    "secret_scanning_push_protection": { "status": "enabled" },
    "secret_scanning_non_provider_patterns": { "status": "enabled" },
    "secret_scanning_validity_checks": { "status": "enabled" }
  }
}
JSON

# Without these, a published CVE in an action that runs alongside the apply role
# produces no alert and no out-of-cycle PR — the only cadence would be the weekly
# version-update run plus the 14-day cooldown, so ~21 days to notice a KNOWN
# vulnerability. The cooldown is right for unknown badness and wrong for known
# badness; security updates are the path that bypasses it.
echo "==> Enabling Dependabot alerts and security updates"
gh api -X PUT "repos/${SLUG}/vulnerability-alerts" >/dev/null
gh api -X PUT "repos/${SLUG}/automated-security-fixes" >/dev/null

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
# not an acceptable default.
#
# sha_pinning_required makes GitHub reject a workflow referencing an action by
# tag rather than commit SHA — enforcing at the platform level what the
# workflows already do by convention, so it cannot regress in review.
echo "==> Restricting allowed actions and enforcing SHA pinning"
gh api -X PUT "repos/${SLUG}/actions/permissions" --input - >/dev/null <<'JSON'
{ "enabled": true, "allowed_actions": "selected", "sha_pinning_required": true }
JSON

# Repository-scoped patterns, NOT exact SHAs — reverted deliberately.
#
# Exact SHAs here looked stricter and were a trap. Every Dependabot SHA bump
# produces a PR whose own workflow references a SHA absent from this list, so
# the run is rejected, the required checks never report, and the PR becomes
# unmergeable by anyone — bypass_actors is empty. The sharpest edge: Dependabot
# security updates are enabled, so a published CVE in configure-aws-credentials
# produces exactly that deadlock. The emergency path is the one that jams.
#
# What the strictness bought was narrow: it stopped a workflow referencing an
# unmerged fork-PR commit inside an allowlisted action's repo. But editing the
# workflow to do that requires repository write access, which is accepted to
# grant the apply role anyway. So it defended against nothing reachable, and
# cost a guaranteed outage on the security-fix path.
#
# Still enforced: sha_pinning_required (workflows must use SHAs, not tags), and
# github_owned_allowed stays false so this is four named repositories rather
# than all of actions/*.
gh api -X PUT "repos/${SLUG}/actions/permissions/selected-actions" --input - >/dev/null <<'JSON'
{
  "github_owned_allowed": false,
  "verified_allowed": false,
  "patterns_allowed": [
    "actions/checkout@*",
    "opentofu/setup-opentofu@*",
    "aws-actions/configure-aws-credentials@*",
    "dependabot/fetch-metadata@*"
  ]
}
JSON

# Workflows get read-only tokens unless they ask for more in their `permissions`
# block. Every workflow in this repo declares exactly what it needs.
echo "==> Restricting default workflow token to read-only"
#
# can_approve_pull_request_reviews is now FALSE. It was true only so the
# (deleted) lock-refresh workflow could `gh pr create` — GitHub gates PR
# creation by Actions behind the same toggle as PR approval. Leaving it on was a
# trap: the setting is "create AND approve", so any workflow holding
# pull-requests: write could POST an approving review as github-actions[bot].
# That is harmless at 0 required approvals, but it would have silently defeated
# the obvious fix of raising the approval count to 1 — the requirement would be
# self-satisfiable from inside a PR, and GitHub's self-approval prohibition does
# not apply to bots.
gh api -X PUT "repos/${SLUG}/actions/permissions/workflow" \
  -F default_workflow_permissions=read \
  -F can_approve_pull_request_reviews=false \
  >/dev/null
echo "    Workflow token read-only; Actions cannot approve pull requests."

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
#
# NO required reviewer, and NO wait timer — deliberately removed.
#
# They were added to put a human between "merge" and "apply". An audit showed
# that was never true here: approving a deployment is an API call authorized by
# the `repo` scope, and the token holding it lives on the same machine as the
# pipeline. An attacker who owns the pipeline reads the token and self-approves
# for the cost of one request and a five-minute wait. The approval records for
# runs 33275353685 and 33276218610 show exactly that shape.
#
# A control that is bypassable by the attacker it is meant to stop, while
# reading as a human gate in the README, is worse than no control: it buys
# false confidence and five minutes per deploy. The accepted position is that
# repository write access grants the apply role. The real containment is the
# IAM blast radius in bootstrap/iam.tf, not anything on the GitHub side.
#
# If a genuine gate is ever wanted, the approver must be an identity the
# pipeline host cannot reach — a separate account whose credentials exist only
# on a phone, with prevent_self_review: true. Any approval path terminating in a
# token on the CI host is theatre by construction.
#
# The branch policy below is NOT theatre and stays: it is what stops a workflow
# on a non-main branch declaring `environment: production` and thereby minting
# an OIDC token whose subject the apply role trusts.
echo "==> Creating '${ENVIRONMENT}' environment (branch-scoped to main; no reviewer)"
gh api -X PUT "repos/${SLUG}/environments/${ENVIRONMENT}" --input - >/dev/null <<'JSON'
{
  "wait_timer": 0,
  "reviewers": [],
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
          { "context": "plan (bootstrap)", "integration_id": 15368 },
          { "context": "plan (infra)", "integration_id": 15368 },
          { "context": "health", "integration_id": 15368 }
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

# ---------------------------------------------------------------------------
# Application deploy targets.
#
# Read from infra outputs rather than hardcoded, so a rebuilt distribution or a
# renamed bucket does not leave the deploy workflow pushing at a resource that
# no longer exists. Skipped silently if infra has never been applied.
# ---------------------------------------------------------------------------
if [ -f "${REPO_ROOT}/infra/backend.hcl" ]; then
  pushd "${REPO_ROOT}/infra" >/dev/null
  if tofu output -raw cloudfront_distribution_id >/dev/null 2>&1; then
    echo "==> Publishing application deploy targets"
    gh variable set SITE_BUCKET               --repo "$SLUG" --body "$(tofu output -raw site_bucket)"
    gh variable set CLOUDFRONT_DISTRIBUTION_ID --repo "$SLUG" --body "$(tofu output -raw cloudfront_distribution_id)"
    gh variable set API_BASE_URL              --repo "$SLUG" --body "$(tofu output -raw api_function_url)"
    echo "    SITE_BUCKET, CLOUDFRONT_DISTRIBUTION_ID, API_BASE_URL"
  else
    echo "==> Skipping deploy targets (infra outputs not present yet)"
  fi
  popd >/dev/null
fi
