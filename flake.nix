{
  description = "Hokum: a freedesktop notification daemon for Wayland, drawn with Rune";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      inherit (nixpkgs) lib;
      crate = (lib.importTOML ./Cargo.toml).package;
    in
    {
      packages = lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          libs = with pkgs; [
            libxkbcommon
            vulkan-loader
            wayland
          ];
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "hokum";
            inherit (crate) version;
            src = ./.;

            # Rune is a git dependency pinned by rev in Cargo.lock, so no hash to keep in step.
            cargoLock = {
              lockFile = ./Cargo.lock;
              allowBuiltinFetchGit = true;
            };

            nativeBuildInputs = with pkgs; [
              pkg-config
              makeWrapper
              rustPlatform.bindgenHook
            ];
            buildInputs = libs ++ [ pkgs.pipewire ];

            postInstall = ''
              substituteInPlace packaging/hokum.service \
                --replace-fail /usr/bin/hokum "$out/bin/hokum" \
                --replace-fail /bin/sh ${pkgs.runtimeShell}
              substituteInPlace packaging/app.hokum.Notifications.service \
                --replace-fail /usr/bin/hokum "$out/bin/hokum"
              install -Dm644 packaging/hokum.service -t "$out/lib/systemd/user"
              install -Dm644 packaging/app.hokum.Notifications.service -t "$out/share/dbus-1/services"
              install -Dm644 packaging/hokum.desktop -t "$out/share/applications"
            '';

            # Vulkan and Wayland are loaded at run time. Rune looks for fallback
            # fonts (emoji) in $XDG_DATA_DIRS/*/fonts; NixOS has them in X11/fonts.
            postFixup = ''
              patchelf --add-rpath ${lib.makeLibraryPath libs} "$out/bin/hokum"
              wrapProgram "$out/bin/hokum" \
                --suffix XDG_DATA_DIRS : /run/current-system/sw/share/X11
            '';

            meta = {
              inherit (crate) description homepage;
              license = lib.licenses.mit;
              mainProgram = "hokum";
              platforms = lib.platforms.linux;
            };
          };
        }
      );
    };
}
