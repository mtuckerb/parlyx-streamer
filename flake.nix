{
  description = "parlyx-streamer dev shell — Tauri 2 + cpal";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        linuxDeps = with pkgs; [
          # cpal / ALSA
          alsa-lib
          # Tauri 2 webview
          webkitgtk_4_1
          gtk3
          glib
          gdk-pixbuf
          cairo
          pango
          atk
          harfbuzz
          libsoup_3
          librsvg
        ];
        common = with pkgs; [
          pkg-config
          openssl
          openssl.dev
          cmake
          cargo
          rustc
          rustfmt
          clippy
          nodejs_20
        ];
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = common ++ pkgs.lib.optionals pkgs.stdenv.isLinux linuxDeps;
          shellHook = ''
            export PKG_CONFIG_PATH=$PKG_CONFIG_PATH
            export NPM_CONFIG_PREFIX=$PWD/.npm-global
            export PATH=$PWD/.npm-global/bin:$PATH
            echo "parlyx-streamer dev shell — run 'npm install' then 'npm run tauri dev'"
          '';
        };
      });
}
