{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  libxkbcommon,
  libnotify,
  fontconfig,
}:

rustPlatform.buildRustPackage {
  pname = "opal";
  version = (lib.importTOML ../Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../build.rs
      ../assets
      ../src
      ../examples
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [ libxkbcommon ];

  # the dev-only input injector in examples/ is not installed
  cargoBuildFlags = [ "--bins" ];
  doCheck = false;

  # notify-send for the "copied" notification, fc-match to find the text tool's font
  postInstall = ''
    wrapProgram $out/bin/opal --prefix PATH : ${lib.makeBinPath [ libnotify fontconfig ]}
  '';

  meta = {
    description = "Screenshot tool for Wayland: region, monitor or all monitors, with annotations, a flat pixel UI and a zoom tool";
    mainProgram = "opal";
    platforms = lib.platforms.linux;
  };
}
