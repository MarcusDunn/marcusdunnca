{
  description = "marcusdunnca — AWS infrastructure managed with OpenTofu";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            opentofu
            awscli2
            gh
            jq
          ];

          # The `marcusdunnca` profile is declared in nix-config
          # (modules/aws.nix), so ~/.aws/config stays the single source of truth.
          # Defaulting AWS_PROFILE here means commands in this shell target the
          # personal account without anyone remembering a flag, while the work
          # profiles on this machine stay one explicit override away.
          #
          # This is convenience, not a guarantee — the real guard against applying
          # to the wrong account is the account-ID assertion in scripts/bootstrap.sh.
          shellHook = ''
            export AWS_PROFILE="marcusdunnca"
            unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
            echo "marcusdunnca devshell — tofu $(tofu version | head -1 | cut -d' ' -f2), profile $AWS_PROFILE"
          '';
        };
      });
    };
}
