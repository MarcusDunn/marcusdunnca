#!/usr/bin/env bash
#
# Commit the given files to a new branch and open a PR, using the GitHub
# GraphQL createCommitOnBranch mutation.
#
# Why GraphQL and not `git commit && git push`: main requires signed commits.
# Commits created through the API are signed by GitHub's own key and land as
# "Verified", whereas a commit made by `git` inside a runner is unsigned and
# would be rejected by the ruleset. This keeps the signing requirement absolute
# — there is no bot signing key to steal, and no bypass actor.
#
# Usage: open-lockfile-pr.sh <branch> <title> <body-file> <file>...
set -euo pipefail

BRANCH="$1"; shift
TITLE="$1"; shift
BODY_FILE="$1"; shift
FILES=("$@")

if [ ${#FILES[@]} -eq 0 ]; then
  echo "No changed files; nothing to do."
  exit 0
fi

: "${GITHUB_REPOSITORY:?must be set}"
BASE_SHA="$(git rev-parse HEAD)"

echo "Creating branch $BRANCH at $BASE_SHA"
gh api "repos/${GITHUB_REPOSITORY}/git/refs" \
  -f ref="refs/heads/${BRANCH}" \
  -f sha="${BASE_SHA}" >/dev/null

# GraphQL wants each file as {path, contents: <base64>}.
additions="$(
  for f in "${FILES[@]}"; do
    jq -n --arg path "$f" --arg contents "$(base64 -w0 "$f")" \
      '{path: $path, contents: $contents}'
  done | jq -s '.'
)"

read -r -d '' QUERY <<'GRAPHQL' || true
mutation($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) {
    commit { oid url }
  }
}
GRAPHQL

variables="$(
  jq -n \
    --arg repo "$GITHUB_REPOSITORY" \
    --arg branch "$BRANCH" \
    --arg headline "$TITLE" \
    --arg oid "$BASE_SHA" \
    --argjson additions "$additions" \
    '{
       input: {
         branch: {
           repositoryNameWithOwner: $repo,
           branchName: $branch
         },
         message: { headline: $headline },
         fileChanges: { additions: $additions },
         expectedHeadOid: $oid
       }
     }'
)"

echo "Creating signed commit on $BRANCH"
jq -n --arg q "$QUERY" --argjson v "$variables" '{query: $q, variables: $v}' \
  | gh api graphql --input - >/dev/null

echo "Opening pull request"
gh pr create \
  --base main \
  --head "$BRANCH" \
  --title "$TITLE" \
  --body-file "$BODY_FILE" \
  --label dependencies
