{
  description = "High-performance review-first terminal diff viewer with PR-style comments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        version = "0.1.1";
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "hewdiff";
          inherit version;

          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          meta = with pkgs.lib; {
            description = "High-performance review-first terminal diff viewer with PR-style comments";
            homepage = "https://github.com/yusukeshib/hew";
            license = licenses.mit;
            mainProgram = "hew";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
          ];
        };
      }
    );
}
