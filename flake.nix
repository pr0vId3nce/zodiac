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
            # zodiac-gui runtime (winit/wgpu dlopen these at run time)
            vulkan-loader
            wayland
            libxkbcommon
          ];
          # dlopen'd libraries for `cargo run -p zodiac-gui`: libvulkan,
          # libwayland-client, libxkbcommon, plus the system Vulkan ICDs
          # (RADV & friends) from /run/opengl-driver on NixOS.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [
            vulkan-loader
            wayland
            libxkbcommon
          ]) + ":/run/opengl-driver/lib";
          shellHook = ''
            echo "zodiac devshell — merge gate: ./scripts/check.sh"
          '';
        };
      });
}
