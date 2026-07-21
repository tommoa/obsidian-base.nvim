{
  description = "Obsidian Bases worker and Neovim integration";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/release-26.05";

  outputs =
    { self, nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          worker = pkgs.rustPlatform.buildRustPackage {
            pname = "obsidian-base-worker";
            version = "0.1.0";
            src = "${self}/worker";
            cargoLock.lockFile = "${self}/worker/Cargo.lock";
            postPatch = ''
              cp -R ${self}/fixtures ../fixtures
              chmod -R u+w ../fixtures
            '';
            doCheck = true;
            postInstall = ''
              test -x "$out/bin/obsidian-base-worker"
            '';
          };
        in
        {
          inherit worker;
          default = worker;
        }
      );
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.clippy
              pkgs.rustc
              pkgs.rustfmt
              pkgs.neovim
              pkgs.curl
              pkgs.coreutils
              pkgs.ripgrep
            ];
          };
        }
      );
      checks = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          worker = self.packages.${system}.worker;
          worker-quality = pkgs.stdenv.mkDerivation {
            pname = "obsidian-base-worker-quality";
            version = worker.version;
            src = "${self}/worker";
            cargoDeps = worker.cargoDeps;
            nativeBuildInputs = [
              pkgs.cargo
              pkgs.clippy
              pkgs.rustc
              pkgs.rustfmt
              pkgs.rustPlatform.cargoSetupHook
            ];
            buildPhase = ''
              runHook preBuild
              cargo fmt --check
              cargo clippy --all-targets --locked -- -D warnings
              runHook postBuild
            '';
            installPhase = ''
              touch "$out"
            '';
          };
        in
        {
          inherit worker worker-quality;
          obsidian-base-integration =
            pkgs.runCommand "obsidian-base-integration"
              {
                nativeBuildInputs = [
                  pkgs.neovim
                  pkgs.ripgrep
                  pkgs.curl
                  pkgs.coreutils
                  pkgs.python3
                ];
              }
              ''
                export HOME="$TMPDIR/home"
                export XDG_CONFIG_HOME="$HOME/.config"
                export XDG_DATA_HOME="$TMPDIR/data"
                export XDG_STATE_HOME="$TMPDIR/state"
                mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"

                cp -R ${self} "$TMPDIR/obsidian-base.nvim"
                chmod -R u+w "$TMPDIR/obsidian-base.nvim"
                test -x ${worker}/bin/obsidian-base-worker
                python "$TMPDIR/obsidian-base.nvim/scripts/metadata-check.py"
                nvim --headless -u NONE -i NONE -l "$TMPDIR/obsidian-base.nvim/scripts/config-smoke.lua"
                nvim --headless -u NONE -i NONE -l "$TMPDIR/obsidian-base.nvim/scripts/picker-smoke.lua"
                nvim --headless -u NONE -i NONE -l "$TMPDIR/obsidian-base.nvim/scripts/native-installer-smoke.lua"
                OBSIDIAN_BASE_WORKER=${worker}/bin/obsidian-base-worker \
                    nvim --headless -u NONE -i NONE -l "$TMPDIR/obsidian-base.nvim/scripts/worker-smoke.lua"
                OBSIDIAN_BASE_WORKER=${worker}/bin/obsidian-base-worker \
                    nvim --headless -u NONE -i NONE -l "$TMPDIR/obsidian-base.nvim/scripts/verify-cli-goldens.lua" \
                    "$TMPDIR/obsidian-base.nvim/fixtures/cli-capture.json" \
                    "$TMPDIR/obsidian-base.nvim/fixtures/vault"
                OBSIDIAN_BASE_WORKER=${worker}/bin/obsidian-base-worker \
                    nvim --headless -u NONE -i NONE -l "$TMPDIR/obsidian-base.nvim/scripts/commands-smoke.lua"
                OBSIDIAN_BASE_WORKER=${worker}/bin/obsidian-base-worker \
                    nvim --headless -u NONE -i NONE -l "$TMPDIR/obsidian-base.nvim/scripts/recovery-smoke.lua"
                sh "$TMPDIR/obsidian-base.nvim/scripts/architecture-check.sh" \
                  "$TMPDIR/obsidian-base.nvim"

                touch "$out"
              '';
        }
      );
    };
}
