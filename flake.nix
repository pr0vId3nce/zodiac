{
  description = "zodiac — a terminal multiplexer built around AI coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages = rec {
          zodiac = pkgs.rustPlatform.buildRustPackage {
            pname = "zodiac";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            # vt100 is vendored in-tree (vendor/vt100), a plain path
            # dependency — no hash overrides needed.
            meta = with pkgs.lib; {
              description = "Terminal multiplexer built around AI coding agents";
              homepage = "https://github.com/pr0vId3nce/zodiac";
              license = licenses.mit;
              mainProgram = "zodiac";
              platforms = platforms.linux ++ platforms.darwin;
            };
          };
          default = zodiac;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustc
            cargo
            clippy
            rustfmt
            # astrolabe bridge/web
            nodejs_24
          ];
        };
      });
}
