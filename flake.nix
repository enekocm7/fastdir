{
  description = "FastDir: blazing fast disk usage analyzer with a treemap";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # `nix run github:enekocm7/fastdir` starts the GUI;
      # `nix run github:enekocm7/fastdir#fastdir-cli -- scan /` the CLI.
      packages = forAllSystems (pkgs: rec {
        fastdir = pkgs.callPackage ./packaging/nix/package.nix { };
        fastdir-cli = pkgs.writeShellScriptBin "fastdir-cli" ''exec ${fastdir}/bin/fastdir-cli "$@"'';
        default = fastdir;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.fastdir ];
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
          ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (
            self.packages.${pkgs.stdenv.hostPlatform.system}.fastdir.runtimeLibs
          );
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
