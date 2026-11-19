# ngteams from its release archive, run in an FHS environment.
#
# The release binaries and the WPE WebKit runtime that the app downloads at its first sign-in are built for an FHS
# system: they use /lib64/ld-linux-x86-64.so.2, and the runtime starts /usr/bin/bwrap and /usr/bin/xdg-dbus-proxy at
# those paths. nixpkgs has no WPE WebKit 2.54. Thus the app runs unchanged in a buildFHSEnv that supplies each of
# these parts. `nixInstall` tells the app how it was installed, so that its update notice gives the right command.
{
  lib,
  stdenv,
  fetchurl,
  buildFHSEnv,
  patchelf,
  nixInstall ? "profile",
}:

assert lib.assertOneOf "nixInstall" nixInstall [
  "profile"
  "nixos"
  "home-manager"
];

let
  release = lib.importJSON ./release.json;
  system = stdenv.hostPlatform.system;
  asset = release.${system} or (throw "ngteams has no release archive for ${system}");
  inherit (release) version;

  meta = {
    description = "Native Microsoft Teams client for Linux";
    homepage = "https://github.com/nickalie/ngteams";
    license = with lib.licenses; [
      mit
      asl20
    ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
    platforms = builtins.filter (name: name != "version") (builtins.attrNames release);
    mainProgram = "ngteams";
  };

  # The archive's tree as the release made it. Only the update marker changes, and ngteamsctl gets a copy that runs
  # outside the FHS environment (it needs only libc and libgcc_s).
  unwrapped = stdenv.mkDerivation {
    pname = "ngteams-unwrapped";
    inherit version meta;
    src = fetchurl { inherit (asset) url hash; };
    nativeBuildInputs = [ patchelf ];
    sourceRoot = "ngteams";
    dontConfigure = true;
    dontBuild = true;
    # the release's own binaries, unstripped and unpatched: the app checks its files as the release made them
    dontFixup = true;
    installPhase = ''
      runHook preInstall
      mkdir -p $out/opt/ngteams $out/bin
      cp -a . $out/opt/ngteams/
      chmod u+w $out/opt/ngteams/usr/share/ngteams/update-format
      echo "nix-${nixInstall}" > $out/opt/ngteams/usr/share/ngteams/update-format
      install -m 0755 usr/bin/ngteamsctl $out/bin/ngteamsctl
      patchelf --set-interpreter "$(cat $NIX_CC/nix-support/dynamic-linker)" \
        --set-rpath ${lib.makeLibraryPath [ stdenv.cc.cc.lib ]} $out/bin/ngteamsctl
      runHook postInstall
    '';
  };

  # the environment that the app runs in; `shell` gives the same environment with a shell, to look into it
  environment = {
    inherit version;
    targetPkgs =
      pkgs:
      (with pkgs; [
        # the app's own libraries, and those it opens at run time
        stdenv.cc.cc.lib
        glib
        glib-networking
        libxkbcommon
        pipewire
        libpulseaudio
        alsa-lib
        wayland
        libglvnd
        libdrm
        (pkgs.libgbm or pkgs.mesa)
        vulkan-loader
        libx11
        libxcb
        libxau
        libxdmcp
        libxcomposite
        libxcursor
        libxdamage
        libxext
        libxfixes
        libxi
        libxrandr
        libxrender
        # the host libraries that the WPE WebKit runtime keeps outside it (its HOST-LIBRARIES file)
        brotli
        bzip2
        e2fsprogs
        expat
        fontconfig
        freetype
        fribidi
        gmp
        graphite2
        harfbuzz
        libbsd
        libffi
        libgpg-error
        libmd
        libpng
        pcre2
        zlib
        # what the runtime starts, at the paths it was built with
        bubblewrap
        xdg-dbus-proxy
        cacert
      ])
      ++ (with pkgs.gst_all_1; [
        gstreamer
        gst-plugins-base
        gst-plugins-good
        gst-plugins-bad
        gst-libav
      ]);

    profile = ''
      export GST_PLUGIN_SYSTEM_PATH_1_0=/usr/lib/gstreamer-1.0
      export GIO_EXTRA_MODULES=/usr/lib/gio/modules
    '';
  };
in
buildFHSEnv (
  environment
  // {
    pname = "ngteams";
    runScript = "${unwrapped}/opt/ngteams/usr/bin/ngteams";

    extraInstallCommands = ''
      ln -s ${unwrapped}/bin/ngteamsctl $out/bin/ngteamsctl
      mkdir -p $out/share
      for part in applications icons bash-completion fish zsh; do
        cp -r ${unwrapped}/opt/ngteams/usr/share/$part $out/share/
      done
    '';

    passthru = {
      inherit unwrapped nixInstall;
      shell = buildFHSEnv (
        environment
        // {
          pname = "ngteams-shell";
          runScript = "bash";
        }
      );
    };

    inherit meta;
  }
)
