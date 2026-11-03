{
  description = "protonaut — GUI-лаунчер игр поверх umu-launcher (чистый Wayland, iced)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" ];

      flake.nixosModules.default = { pkgs, ... }: {
        environment.systemPackages = [ self.packages.${pkgs.system}.protonaut ];
      };

      perSystem =
        {
          pkgs,
          lib,
          config,
          ...
        }:
        {
          packages.protonaut = pkgs.callPackage ./nix/package.nix {
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./src
                ./assets
                ./README.md
                ./LICENSE
              ];
            };
          };

          packages.default = config.packages.protonaut;

          # cargo build без Nix: пути dlopen-библиотек берутся из пакета
          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              pkg-config
              gcc
              clang
            ];
            buildInputs =
              with pkgs;
              [
                alsa-lib
                fontconfig
                freetype
                libGL
                libdrm
                libxkbcommon
                mesa
                udev
                vulkan-headers
                vulkan-loader
                wayland
              ]
              ++ [ pkgs.umu-launcher ];
            shellHook = ''
              export LD_LIBRARY_PATH=/run/opengl-driver/lib:${config.packages.protonaut.passthru.runtimePath}
            '';
          };

          formatter = pkgs.nixfmt;
        };
    };
}
