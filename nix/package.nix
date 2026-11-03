{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  makeDesktopItem,
  alsa-lib,
  fontconfig,
  freetype,
  libGL,
  libdrm,
  libx11,
  libxcursor,
  libxfixes,
  libxi,
  libxkbcommon,
  libxrandr,
  libxrender,
  mesa,
  udev,
  wayland,
  vulkan-loader,
  umu-launcher,
  # В дереве flake источник передаётся явно (fileset); для подачи в nixpkgs этот
  # аргумент заменяют на fetchFromGitHub прямо в файле.
  src ? ../.,
}:

let
  version = "0.1.0";

  # iced тянет winit/softbuffer, которые грузят библиотеки через dlopen:
  # без этого пути окно не появится ни на Wayland, ни на X11. X11-клиентские
  # библиотеки нужны именно потому, что сессия может быть иксова (или XWayland):
  # без libX11 winit падает ещё до первого кадра.
  runtimePath = lib.makeLibraryPath [
    vulkan-loader
    mesa
    libdrm
    libxkbcommon
    wayland
    alsa-lib
    fontconfig
    freetype
    udev
    libGL
    libx11
    libxcursor
    libxfixes
    libxi
    libxrandr
    libxrender
  ];

  desktopItem = makeDesktopItem {
    name = "umulaunch";
    type = "Application";
    desktopName = "umulaunch";
    genericName = "Game launcher";
    comment = "Launch Windows games through umu-launcher";
    terminal = false;
    exec = "umulaunch";
    icon = "applications-games";
    categories = [
      "Game"
      "Utility"
    ];
    startupWMClass = "umulaunch";
  };
in
rustPlatform.buildRustPackage {
  pname = "umulaunch";
  inherit version src;

  cargoHash = "sha256-ZPtg3uaLG8jjmhr5JmBRxZT0/UjPkzaqNSXu0wvpGt8=";

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];

  buildInputs = [
    alsa-lib
    fontconfig
    libdrm
    libxkbcommon
    mesa
    udev
    wayland
  ];

  # Тесты не требуют ни дисплея, ни сети: разбор конфига, слаг, аргументы,
  # экранирование и остановка дерева процессов (тот спавнит `sh` в песочнице).
  doCheck = true;

  postInstall = ''
    install -D ${desktopItem}/share/applications/umulaunch.desktop \
      $out/share/applications/umulaunch.desktop

    wrapProgram $out/bin/umulaunch \
      --prefix LD_LIBRARY_PATH : "${runtimePath}:/run/opengl-driver/lib" \
      --prefix PATH : ${lib.makeBinPath [ umu-launcher ]}
  '';

  # devShell из flake берёт отсюда путь для LD_LIBRARY_PATH.
  passthru.runtimePath = runtimePath;

  meta = {
    description = "Wayland game launcher that runs Windows games through umu-launcher";
    longDescription = ''
      Minimal library launcher: it discovers Proton runners installed by
      Steam, Lutris or Heroic, writes a TOML config, and starts each game as
      `umu-run <exe>` with GAMEID/STORE/WINEPREFIX/PROTONPATH and the
      STEAM_COMPAT_* variables Proton needs to map a real drive letter.
      It can also download a GE-Proton release, verify the published checksum
      and install it into Steam's compatibilitytools.d, where both Steam and
      this launcher pick it up.
      Also has --list / --runners / --dry-run / --launch / --proton-list /
      --proton-install for the terminal.
    '';
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "umulaunch";
    # maintainer = [ ];  # заполнить при подаче в nixpkgs
  };
}
