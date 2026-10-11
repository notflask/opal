{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  libxkbcommon,
  libnotify,
  wl-clipboard,
}:

rustPlatform.buildRustPackage {
  pname = "opal";
  version = (lib.importTOML ../Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
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

  # notify-send for the "copied" notification
  postInstall = ''
    wrapProgram $out/bin/opal --prefix PATH : ${lib.makeBinPath [ libnotify ]}
  '';

  meta = {
    description = "Screenshot tool for Wayland: region, monitor or all monitors, with annotations and a liquid-glass UI";
    mainProgram = "opal";
    platforms = lib.platforms.linux;
  };
}
