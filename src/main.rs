use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use ashpd::PortalError;
use ashpd::desktop::ResponseError;
use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use iced::border::Radius;
use iced::keyboard::{self, Modifiers};
use iced::widget::{
    Column, Row, Space, TextInput, button, container, pick_list, progress_bar, scrollable, text,
    text_input,
};
use iced::{
    Alignment, Background, Border, Color, Element, Length, Padding, Shadow, Size, Subscription,
    Task, Theme, Vector,
};
use serde::{Deserialize, Serialize};

const APP: &str = "umulaunch";
const STORES: [&str; 6] = ["none", "heroic", "gog", "epicgames", "legendary", "other"];

// Палитра. Тема одна, светлого варианта нет — цвета можно держать константами.
const BG: Color = Color::from_rgb(0.075, 0.082, 0.102);
/// Дно утопленных блоков: команды, лога, списка установленных раннеров.
const BG_DEEP: Color = Color::from_rgb(0.051, 0.055, 0.07);
const CARD: Color = Color::from_rgb(0.102, 0.114, 0.137);
const CARD_HOVER: Color = Color::from_rgb(0.129, 0.145, 0.176);
const LINE: Color = Color::from_rgb(0.184, 0.2, 0.239);
const TEXT: Color = Color::from_rgb(0.902, 0.91, 0.937);
const MUTED: Color = Color::from_rgb(0.545, 0.565, 0.639);
const PRIMARY: Color = Color::from_rgb(0.435, 0.549, 1.0);
/// Цвет пилюли с именем раннера: отличим от статуса, но не кричит.
const RUNNER_TINT: Color = Color::from_rgb(0.62, 0.74, 0.95);
const SUCCESS: Color = Color::from_rgb(0.306, 0.796, 0.443);
const WARNING: Color = Color::from_rgb(0.878, 0.639, 0.333);
const DANGER: Color = Color::from_rgb(0.878, 0.396, 0.373);

fn theme() -> Theme {
    Theme::custom(
        APP,
        iced::theme::Palette {
            background: BG,
            text: TEXT,
            primary: PRIMARY,
            success: SUCCESS,
            warning: WARNING,
            danger: DANGER,
        },
    )
}

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(cli) = Cli::parse(&args) {
        return cli.run();
    }
    iced::application(State::new, update, view)
        .title(APP)
        .theme(theme())
        .subscription(subscription)
        .window_size(Size::new(1280.0, 840.0))
        // Закрытие обрабатываем сами: сначала пишем конфиг, потом выходим.
        // Иначе runtime убивает окно раньше, чем успевает дойти Message::Quit.
        .exit_on_close_request(false)
        .run()
}

// ---------------------------------------------------------------- CLI
//
// Без GUI тот же конфиг можно прочитать и запустить игру из терминала.

enum Cli {
    Help,
    Version,
    List,
    Runners,
    DryRun(String),
    Launch(String),
    ProtonList,
    ProtonInstall(String),
}

const USAGE: &str = "\
umulaunch — запуск Windows-игр через umu-launcher

Без аргументов открывает окно (Wayland). Конфиг ищется в порядке
$UMULAUNCH_CONFIG, $XDG_CONFIG_HOME/umulaunch/config.toml.

Флаги:
  --list             игры из конфига с развёрнутыми значениями
  --runners          каталоги Proton, которые видно в окне
  --dry-run <ИМЯ>    напечатать команду запуска, ничего не запуская
  --launch <ИМЯ>     запустить из терминала и ждать выхода (для скриптов)
  --proton-list      версии GE-Proton, доступные для загрузки (нужна сеть)
  --proton-install <ТЕГ>
                     скачать GE-Proton, сверить хеш и положить в compatibilitytools.d
  -h, --help         этот текст
  -V, --version      версия

Имя игры можно задавать частично: хватит подстроки, точное совпадение приоритетнее.
--proton-install качает в $XDG_DATA_HOME/Steam/compatibilitytools.d, распаковку
пишет во временный каталог и сверяет контрольную сумму, если релиз её публикует.
Код возврата: 0 успех, 1 ошибка конфига, сети или запуска, 2 неверный вызов.

Схема конфига (<> — заполнить собой, остальное необязательное):
  umu_run = \"<путь к umu-run>\"
  [defaults]
  runner = \"<каталог Proton>\"        # пусто — решает umu-launcher
  store = \"<none|heroic|gog|epicgames|legendary|other>\"
  prefix_root = \"~/Games/umu\"        # префиксы вида <prefix_root>/<GAMEID>
  runner_dirs = [\"<папка с раннерами>\"]
  [[game]]
  name = \"<имя игры>\"                # обязательно
  exe = \"<путь к .exe>\"              # обязательно, ~/ разворачивается
  gameid = \"<steam-AppID или um-…>\"  # пусто — слаг из имени
  prefix = \"<каталог префикса>\"      # пусто — <prefix_root>/<GAMEID>
  store = \"<store для игры>\"
  runner = \"<каталог Proton для игры>\"
  args = [\"<аргумент>\", \"<ещё>\"]
  [game.env]
  \"<ПЕРЕМЕННАЯ>\" = \"<значение>\"

Логи игр: $XDG_STATE_HOME/umulaunch/<имя-игры>.log\n";

impl Cli {
    fn parse(args: &[String]) -> Option<Self> {
        let flag = args.first().map(|a| a.as_str())?;
        match flag {
            "--help" | "-h" => Some(Cli::Help),
            "--version" | "-V" => Some(Cli::Version),
            "--list" | "--runners" => Some(if flag == "--list" {
                Cli::List
            } else {
                Cli::Runners
            }),
            "--proton-list" => Some(Cli::ProtonList),
            "--proton-install" => {
                let tag = args.get(1).cloned().unwrap_or_default();
                if tag.is_empty() {
                    eprintln!("нужен тег релиза: umulaunch --proton-install \"GE-Proton…\"");
                    std::process::exit(2);
                }
                Some(Cli::ProtonInstall(tag))
            }
            "--dry-run" | "--launch" => {
                let name = args.get(1).cloned().unwrap_or_default();
                if name.is_empty() {
                    eprintln!("нужно имя игры: umulaunch {flag} \"<имя игры>\"");
                    std::process::exit(2);
                }
                Some(if flag == "--dry-run" {
                    Cli::DryRun(name)
                } else {
                    Cli::Launch(name)
                })
            }
            _ if flag.starts_with('-') => {
                eprintln!("неизвестный флаг: {flag}\n\n{USAGE}");
                std::process::exit(2);
            }
            _ => None,
        }
    }

    fn run(self) -> iced::Result {
        match self {
            Cli::Help => {
                print!("{USAGE}");
                return Ok(());
            }
            Cli::Version => {
                println!("umulaunch {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            // загрузка Proton не зависит от конфига: сломанный config.toml
            // не должен мешать поставить раннер
            Cli::ProtonList => {
                proton_list();
                return Ok(());
            }
            Cli::ProtonInstall(tag) => {
                proton_install(&tag);
                return Ok(());
            }
            _ => {}
        }
        let state = State::new();
        if let Some(e) = &state.error {
            eprintln!("конфиг: {e}");
            std::process::exit(1);
        }
        match self {
            Cli::List => {
                println!("конфиг:    {}", config_path().display());
                println!("umu-run:   {}", state.umu.display());
                println!("префиксы:  {}", expand(&state.prefix_root_input).display());
                println!("игр:       {}", state.games.len());
                for g in &state.games {
                    println!("  {}", describe(&g.cfg, &state.defaults));
                }
            }
            Cli::Runners => {
                println!("раннеры (каталоги, где лежит исполняемый `proton`):");
                for r in state.runners.iter().skip(1) {
                    println!("  {}", r.path.clone().unwrap_or_default());
                }
                println!(
                    "дополнительные каталоги из конфига: {:?}",
                    state.defaults.runner_dirs
                );
            }
            Cli::DryRun(name) => match find(&state.games, &name) {
                Some((i, g)) => match plan(&g.cfg, &state.umu, &state.defaults) {
                    Ok(desc) => {
                        println!("# игра №{i}: {}", g.cfg.name);
                        println!("{desc}");
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        std::process::exit(1);
                    }
                },
                None => not_found(&name),
            },
            Cli::Launch(name) => match find(&state.games, &name).map(|(i, _)| i) {
                Some(i) => {
                    let (umu, defaults) = (state.umu.clone(), state.defaults.clone());
                    let log = state.games[i].log.clone();
                    if let Ok(desc) = plan(&state.games[i].cfg, &umu, &defaults) {
                        println!("# игра №{i}: {}", state.games[i].cfg.name);
                        println!("{desc}");
                    }
                    match launch(&state.games[i], &umu, &defaults) {
                        Ok(mut child) => {
                            println!("запущено — лог {}", log.display());
                            // в терминале ждать ребёнка напрямую: так скрипт получает
                            // его код выхода, а не наш статус
                            let status = match child.wait() {
                                Ok(s) => s,
                                Err(e) => {
                                    eprintln!("не смог дождаться процесса: {e}");
                                    std::process::exit(1);
                                }
                            };
                            match status.code() {
                                Some(c) => {
                                    println!("завершена: код выхода {c}");
                                    std::process::exit(c);
                                }
                                None => {
                                    println!("завершена: убита сигналом");
                                    std::process::exit(1);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("{e}");
                            std::process::exit(1);
                        }
                    }
                }
                None => not_found(&name),
            },
            Cli::Help | Cli::Version | Cli::ProtonList | Cli::ProtonInstall(_) => unreachable!(),
        }
        Ok(())
    }
}

fn not_found(name: &str) -> ! {
    eprintln!("игра «{name}» не найдена — см. --list");
    std::process::exit(1);
}

fn find<'a>(games: &'a [Game], name: &str) -> Option<(usize, &'a Game)> {
    let needle = name.to_lowercase();
    games
        .iter()
        .enumerate()
        .find(|(_, g)| g.cfg.name.to_lowercase() == needle)
        .or_else(|| {
            games
                .iter()
                .enumerate()
                .find(|(_, g)| g.cfg.name.to_lowercase().contains(&needle))
        })
}

fn describe(cfg: &GameCfg, defaults: &Defaults) -> String {
    match resolve(cfg, defaults) {
        Ok((exe, prefix, gameid, store, runner)) => format!(
            "{:<34} runner={:<30} store={:<9} gameid={:<22} prefix={} exe={}",
            clip(&cfg.name, 34),
            runner
                .map(|r| clip(
                    &Path::new(&r)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(r),
                    30
                ))
                .unwrap_or_else(|| "авто".into()),
            clip(&store, 9),
            clip(&gameid, 22),
            prefix.display(),
            exe.display()
        ),
        Err(e) => format!("{:<34} ошибка: {e}", clip(&cfg.name, 34)),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

// ---------------------------------------------------------------- configuration

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Config {
    /// Путь к umu-run. Пусто — искать в PATH и в типовых местах установки.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    umu_run: Option<String>,
    #[serde(default)]
    defaults: Defaults,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    game: Vec<GameCfg>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Defaults {
    /// Раннер по умолчанию (каталог Proton). Пусто — решает umu-launcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runner: Option<String>,
    /// Значение STORE для игр, где оно не указано.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store: Option<String>,
    /// Каталог для префиксов вида <prefix_root>/<GAMEID>.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix_root: Option<String>,
    /// Дополнительные каталоги, где искать раннеры.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    runner_dirs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GameCfg {
    name: String,
    exe: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gameid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    store: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
}

/// Раннер: path=None — «пусть umu-launcher сам решает» (обычно Proton-UMU).
#[derive(Debug, Clone, PartialEq)]
struct Runner {
    path: Option<String>,
}

impl Runner {
    /// Тот же ли это раннер, что и другой, с учётом `~/` и симлинков
    /// (Steam-каталог обычно виден по двум путям сразу).
    fn same(&self, path: PathBuf) -> bool {
        let Some(mine) = &self.path else { return false };
        let mine = expand(mine);
        match (mine.canonicalize(), path.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => mine == path,
        }
    }

    fn canonical(&self) -> Option<PathBuf> {
        self.path
            .as_deref()
            .map(|p| expand(p).canonicalize().unwrap_or_else(|_| expand(p)))
    }
}

impl fmt::Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.path {
            None => write!(f, "авто — решает umu-launcher"),
            Some(p) => write!(
                f,
                "{}",
                Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.clone())
            ),
        }
    }
}

// ---------------------------------------------------------------- runtime state

struct Game {
    cfg: GameCfg,
    child: Option<Child>,
    started: Option<Instant>,
    exit: Option<String>,
    log: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct Draft {
    name: String,
    exe: String,
    runner: Option<String>,
    prefix: String,
    gameid: String,
    store: String,
    args: String,
    env: String,
}

/// Какое поле лаунчера заполняет системный диалог. Файловые цели показывают ещё
/// и файлы, каталожные — только каталоги.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickTarget {
    DraftExe,
    DraftPrefix,
    PrefixRoot,
    UmuRun,
    RunnerDirs,
    ScanDir,
}

impl PickTarget {
    fn title(self) -> &'static str {
        match self {
            Self::DraftExe => "исполняемый файл .exe",
            Self::DraftPrefix => "wine-префикс",
            Self::PrefixRoot => "корень wine-префиксов",
            Self::UmuRun => "umu-run",
            Self::RunnerDirs => "папка с раннерами",
            Self::ScanDir => "папка библиотеки Steam",
        }
    }

    fn wants_file(self) -> bool {
        matches!(self, Self::DraftExe | Self::UmuRun)
    }
}

/// Релиз GE-Proton, пригодный к установке на этой машине: архив нужной
/// архитектуры и, если релиз его публикует, файл контрольной суммы.
#[derive(Debug, Clone, PartialEq)]
struct Release {
    tag: String,
    url: String,
    sum: Option<(String, Algo)>,
    size: u64,
    date: String,
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} · {}", self.tag, human_size(self.size))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Algo {
    Sha512,
    Sha256,
}

impl Algo {
    fn label(self) -> &'static str {
        match self {
            Algo::Sha512 => "sha512",
            Algo::Sha256 => "sha256",
        }
    }

    fn hex_len(self) -> usize {
        match self {
            Algo::Sha512 => 128,
            Algo::Sha256 => 64,
        }
    }
}

/// Стадии установки: атом — только индекс, чтобы worker мог писать в него.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    Download,
    Checksum,
    Verify,
    Unpack,
}

impl Stage {
    fn index(self) -> u8 {
        match self {
            Stage::Download => 0,
            Stage::Checksum => 1,
            Stage::Verify => 2,
            Stage::Unpack => 3,
        }
    }

    fn from_index(v: u8) -> Stage {
        match v {
            1 => Stage::Checksum,
            2 => Stage::Verify,
            3 => Stage::Unpack,
            _ => Stage::Download,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Stage::Download => "скачивание",
            Stage::Checksum => "чтение контрольной суммы",
            Stage::Verify => "сравнение хеша",
            Stage::Unpack => "распаковка",
        }
    }
}

/// Счётчики текущего скачивания: worker пишет сюда, интерфейс читает на тиках.
#[derive(Debug, Default)]
struct DlProgress {
    written: AtomicU64,
    /// 0 — размер неизвестен, полосу прогресса тогда не показываем.
    total: AtomicU64,
    stage: AtomicU8,
    cancel: AtomicBool,
}

impl DlProgress {
    fn add(&self, n: u64) {
        self.written.fetch_add(n, Ordering::Relaxed);
    }

    fn set_total(&self, n: u64) {
        self.total.store(n, Ordering::Relaxed);
    }

    fn set_stage(&self, stage: Stage) {
        self.stage.store(stage.index(), Ordering::Relaxed);
    }

    fn stage(&self) -> Stage {
        Stage::from_index(self.stage.load(Ordering::Relaxed))
    }

    fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    fn total(&self) -> Option<u64> {
        match self.total.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Доля выполненного для progress_bar; None — когда размер неизвестен.
    fn fraction(&self) -> Option<f32> {
        self.total().map(|t| {
            if t == 0 {
                0.0
            } else {
                (self.written() as f64 / t as f64).clamp(0.0, 1.0) as f32
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
enum DlStatus {
    Idle,
    Listing,
    Working,
}

#[derive(Debug, Clone)]
enum Message {
    Reload,
    Save,
    /// Закрытие окна: библиотека нужна и через неделю, поэтому конфиг пишем
    /// сами, а не ждём, пока человек вспомнит про Ctrl+S.
    Quit,
    New,
    Start(usize),
    Stop(usize),
    Select(usize),
    Edit(usize),
    Delete(usize),
    Tick,
    Filter(String),
    ScanDir(String),
    Scan,
    DefaultRunner(Runner),
    DefaultStore(String),
    PrefixRoot(String),
    UmuRun(String),
    RunnerDirs(String),
    DraftName(String),
    DraftExe(String),
    DraftPrefix(String),
    DraftGameId(String),
    DraftStore(String),
    DraftRunner(Runner),
    DraftArgs(String),
    DraftEnv(String),
    Commit,
    Cancel,
    /// Панель загрузки Proton: открыть/закрыть.
    DlToggle,
    /// Свернуть/развернуть карточку параметров по умолчанию.
    SettingsToggle,
    /// Открыть системный диалог выбора пути для поля.
    Pick(PickTarget),
    /// Ответ портала: путь, отмена или ошибка.
    Picked(PickTarget, Result<Option<PathBuf>, String>),
    /// Опросить GitHub и показать список версий.
    DlList,
    DlChosen(Release),
    DlInstall,
    DlCancel,
    DlListed(Result<Vec<Release>, String>),
    DlDone(Result<PathBuf, String>),
    /// Клавиатурные события нужны для Ctrl+S / Ctrl+R / Esc.
    Key(keyboard::Event),
}

struct State {
    games: Vec<Game>,
    error: Option<String>,
    notice: Option<String>,
    warn: Option<String>,
    umu: PathBuf,
    umu_input: String,
    defaults: Defaults,
    prefix_root_input: String,
    runner_dirs_input: String,
    /// Значения pick_list: найденные раннеры плюс выбранный, даже если он пропал.
    runners: Vec<Runner>,
    filter: String,
    selected: Option<usize>,
    /// Игра, у которой подтверждают удаление повторной кнопкой.
    confirm_delete: Option<usize>,
    draft: Option<Draft>,
    scan_dir: String,
    /// Панель «Загрузка Proton» открыта.
    dl_open: bool,
    dl_releases: Vec<Release>,
    dl_chosen: Option<Release>,
    dl_status: DlStatus,
    /// Пока задано — установка идёт; по этому же полю тикает прогресс.
    dl_job: Option<Arc<DlProgress>>,
    /// Каталоги раннеров, уже лежащие в папке установки.
    dl_installed: Vec<String>,
    /// Куда разложен последний установленный раннер — по нему предлагают
    /// сделать раннер по умолчанию.
    dl_last: Option<PathBuf>,
    dl_error: Option<String>,
    /// Карточка параметров по умолчанию развёрнута: она съедает треть окна,
    /// а правят её редко.
    settings_open: bool,
}

impl State {
    fn new() -> Self {
        let mut state = State {
            games: Vec::new(),
            error: None,
            notice: None,
            warn: None,
            umu: PathBuf::from("umu-run"),
            umu_input: String::new(),
            defaults: Defaults::default(),
            prefix_root_input: "~/Games/umu".to_string(),
            runner_dirs_input: String::new(),
            runners: vec![Runner { path: None }],
            filter: String::new(),
            selected: None,
            confirm_delete: None,
            draft: None,
            scan_dir: String::new(),
            dl_open: false,
            dl_releases: Vec::new(),
            dl_chosen: None,
            dl_status: DlStatus::Idle,
            dl_job: None,
            dl_installed: installed_runners(&proton_install_dir()),
            dl_last: None,
            dl_error: None,
            settings_open: true,
        };
        state.reload();
        // Конфиг с играми означает «параметры уже настроены»: сворачиваем
        // карточку и отдаём место списку.
        state.settings_open = state.games.is_empty();
        state
    }

    fn reload(&mut self) {
        match load() {
            Ok((config, umu)) => {
                // перечитывание конфига не должно терять уже запущенные игры
                let mut keep: BTreeMap<String, (Option<Child>, Option<Instant>)> =
                    std::mem::take(&mut self.games)
                        .into_iter()
                        .filter_map(|g| g.child.map(|c| (g.cfg.exe, (Some(c), g.started))))
                        .collect();
                self.error = None;
                self.warn = None;
                self.umu = umu;
                self.umu_input = config.umu_run.clone().unwrap_or_default();
                self.defaults = config.defaults.clone();
                self.prefix_root_input = config
                    .defaults
                    .prefix_root
                    .clone()
                    .unwrap_or_else(|| "~/Games/umu".into());
                self.runner_dirs_input = config.defaults.runner_dirs.join("; ");
                self.runners = discover_runners(&config.defaults.runner_dirs);
                self.dl_installed = installed_runners(&proton_install_dir());
                self.games = config
                    .game
                    .into_iter()
                    .map(|cfg| {
                        let (child, started) = keep.remove(&cfg.exe).unwrap_or((None, None));
                        Game {
                            log: log_path(&cfg.name),
                            cfg,
                            child,
                            started,
                            exit: None,
                        }
                    })
                    .collect();
                self.confirm_delete = None;
                self.selected = if self.games.is_empty() {
                    None
                } else {
                    Some(self.selected.unwrap_or(0).min(self.games.len() - 1))
                };
                let chosen: Vec<Option<String>> = self
                    .games
                    .iter()
                    .map(|g| g.cfg.runner.clone())
                    .chain([self.defaults.runner.clone()])
                    .collect();
                for path in chosen {
                    self.ensure_runner(&path);
                }
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn ensure_runner(&mut self, path: &Option<String>) {
        let Some(p) = path else { return };
        if self.runners.iter().any(|r| r.same(expand(p))) {
            return;
        }
        self.runners.push(Runner {
            path: Some(p.clone()),
        });
        self.runners.sort_by(|a, b| a.path.cmp(&b.path));
    }

    fn config(&self) -> Config {
        Config {
            umu_run: none_if_empty(&self.umu_input),
            defaults: Defaults {
                runner: self.defaults.runner.clone(),
                store: self.defaults.store.clone(),
                prefix_root: none_if_empty(&self.prefix_root_input),
                runner_dirs: split_dirs(&self.runner_dirs_input),
            },
            game: self.games.iter().map(|g| g.cfg.clone()).collect(),
        }
    }

    fn save(&mut self) -> Result<PathBuf, String> {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let result = match toml::to_string_pretty(&self.config()) {
            Ok(raw) => {
                // через временный файл: оборвавшаяся запись не должна съедать конфиг
                let tmp = path.with_extension("toml.tmp");
                std::fs::write(&tmp, raw)
                    .and_then(|()| std::fs::rename(&tmp, &path))
                    .map(|()| path.clone())
                    .map_err(|e| format!("не смог записать {}: {e}", path.display()))
            }
            Err(e) => Err(format!("не смог сериализовать конфиг: {e}")),
        };
        match &result {
            Ok(_) => {
                self.notice = Some(format!("сохранено в {}", path.display()));
                self.error = None;
                self.warn = None;
            }
            Err(e) => self.error = Some(e.clone()),
        }
        result
    }

    /// Текущее значение поля, для которого открыли диалог: по нему портал
    /// стартует в нужную папку, а не в домашний каталог.
    fn pick_value(&self, target: PickTarget) -> String {
        match target {
            PickTarget::DraftExe => self
                .draft
                .as_ref()
                .map(|d| d.exe.clone())
                .unwrap_or_default(),
            PickTarget::DraftPrefix => self
                .draft
                .as_ref()
                .map(|d| d.prefix.clone())
                .unwrap_or_default(),
            PickTarget::PrefixRoot => self.prefix_root_input.clone(),
            PickTarget::UmuRun => self.umu_input.clone(),
            // В поле список через «;»: стартуем с последней папки.
            PickTarget::RunnerDirs => split_dirs(&self.runner_dirs_input)
                .pop()
                .unwrap_or_default(),
            PickTarget::ScanDir => self.scan_dir.clone(),
        }
    }

    /// Выбранный путь уходит в поле, которое открыло диалог.
    fn apply_pick(&mut self, target: PickTarget, path: PathBuf) {
        let text = path.display().to_string();
        match target {
            PickTarget::DraftExe => {
                if let Some(d) = self.draft.as_mut() {
                    d.exe = text;
                }
            }
            PickTarget::DraftPrefix => {
                if let Some(d) = self.draft.as_mut() {
                    d.prefix = text;
                }
            }
            PickTarget::PrefixRoot => self.prefix_root_input = text,
            PickTarget::UmuRun => {
                self.umu_input = text;
                self.umu = expand(&self.umu_input);
            }
            PickTarget::RunnerDirs => {
                let mut dirs = split_dirs(&self.runner_dirs_input);
                if !dirs.iter().any(|d| expand(d) == path) {
                    dirs.push(text);
                }
                self.runner_dirs_input = dirs.join("; ");
                self.runners = discover_runners(&dirs);
            }
            PickTarget::ScanDir => self.scan_dir = text,
        }
    }

    fn open_draft(&mut self, index: Option<usize>) {
        let draft = match index.and_then(|i| self.games.get(i)) {
            Some(g) => Draft {
                name: g.cfg.name.clone(),
                exe: g.cfg.exe.clone(),
                runner: g.cfg.runner.clone(),
                prefix: g.cfg.prefix.clone().unwrap_or_default(),
                gameid: g.cfg.gameid.clone().unwrap_or_default(),
                store: g.cfg.store.clone().unwrap_or_else(|| "none".into()),
                args: g.cfg.args.join(" "),
                env: g
                    .cfg
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(";"),
            },
            None => Draft {
                store: self.defaults.store.clone().unwrap_or_else(|| "none".into()),
                ..Default::default()
            },
        };
        self.ensure_runner(&draft.runner);
        self.draft = Some(draft);
        self.selected = index;
        // правая панель одна: карточка игры вытесняет панель загрузки,
        // иначе кнопка «Proton» была бы нажата, а содержимое не менялось
        self.dl_open = false;
    }

    fn apply_draft(&mut self) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        if draft.name.trim().is_empty() || draft.exe.trim().is_empty() {
            self.error = Some("нужно заполнить имя и путь к .exe".to_string());
            return;
        }
        let editing = self.selected;
        let name = draft.name.trim().to_string();
        // Имя задаёт имя файла лога и GAMEID по умолчанию, поэтому два раза
        // одному быть нельзя.
        if self
            .games
            .iter()
            .enumerate()
            .any(|(i, g)| Some(i) != editing && g.cfg.name.to_lowercase() == name.to_lowercase())
        {
            self.error = Some(format!("игра «{name}» уже есть в списке"));
            return;
        }
        let cfg = GameCfg {
            name,
            exe: draft.exe.trim().to_string(),
            runner: draft.runner.filter(|s| !s.trim().is_empty()),
            prefix: none_if_empty(&draft.prefix),
            gameid: none_if_empty(&draft.gameid),
            store: none_if_empty(&draft.store),
            args: split_args(&draft.args),
            env: parse_env(&draft.env),
        };
        match editing {
            Some(i) if i < self.games.len() => {
                if let Some(g) = self.games.get_mut(i) {
                    g.log = log_path(&cfg.name);
                    g.cfg = cfg.clone();
                }
            }
            _ => {
                let log = log_path(&cfg.name);
                self.games.push(Game {
                    cfg: cfg.clone(),
                    child: None,
                    started: None,
                    exit: None,
                    log,
                });
                self.selected = Some(self.games.len() - 1);
            }
        }
        self.confirm_delete = None;
        self.draft = None;
        self.error = None;
        if expand(&cfg.exe).is_file() {
            self.notice =
                Some("изменение применено — «сохранить» запишет его в конфиг".to_string());
        } else {
            self.warn = Some(format!(
                "применено, но {} не найден — запуск появится вместе с файлом",
                cfg.exe
            ));
        }
    }

    fn selected_cfg(&self) -> Option<&GameCfg> {
        self.selected
            .and_then(|i| self.games.get(i))
            .map(|g| &g.cfg)
    }

    fn log_text(&self) -> String {
        match self.selected.and_then(|i| self.games.get(i)) {
            Some(g) if g.log.exists() => tail(&g.log, 16000),
            Some(g) => format!("лога пока нет — он появится в {}", g.log.display()),
            None => "выберите игру, чтобы увидеть её лог".to_string(),
        }
    }
}

fn none_if_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

// ---------------------------------------------------------------- paths and files

fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("UMULAUNCH_CONFIG") {
        return expand(&p);
    }
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP);
    let new = dir.join("config.toml");
    let old = dir.join("games.toml");
    // старое имя подключаем, только если оно реально есть: иначе показываем
    // тот путь, который создаст «сохранить»
    if new.exists() || !old.exists() {
        new
    } else {
        old
    }
}

fn log_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| PathBuf::from(".umulaunch"))
        .join(APP)
}

fn log_path(name: &str) -> PathBuf {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}.log", slug(name)))
}

fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// `~/…` разворачиваем сами — конфиг читают и GUI, и CLI.
fn expand(path: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    if path == "~" {
        PathBuf::from(home)
    } else if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(path)
    }
}
/// Каталог-список из одного поля: `~/a; ~/b` → два пути.
fn split_dirs(s: &str) -> Vec<String> {
    s.split([';', '\n'])
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect()
}

// ------------------------------------------------------------ path picking
//
// Обзор пути — системный диалог, который рисует портал рабочего стола
// (org.freedesktop.portal.FileChooser): тот же window-picker, что у остальных
// приложений, с закладками, поиском и вводом пути. Лаунчер сам ничего не
// перечитывает и своего «проводника» не показывает.

/// С чего начинать диалог: ближайшее существующее каталога значения поля. Если
/// путь указывает на файл — заходим в его каталог.
fn start_dir(value: &str) -> PathBuf {
    let p = expand(value.trim());
    let mut cur = if p.is_file() {
        p.parent().map(Path::to_path_buf).unwrap_or(p)
    } else {
        p
    };
    while !cur.is_dir() {
        let Some(parent) = cur.parent() else { break };
        cur = parent.to_path_buf();
    }
    if cur.is_dir() {
        cur
    } else {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }
}

/// Фильтры диалога: список того, что вообще стоит выбирать в этом поле. Для
/// каталогов фильтров нет — портал и так показывает только папки.
fn pick_filters(target: PickTarget) -> Vec<FileFilter> {
    match target {
        PickTarget::DraftExe => vec![
            FileFilter::new("Windows-программы").glob("*.exe"),
            FileFilter::new("все файлы").glob("*"),
        ],
        PickTarget::UmuRun => vec![
            FileFilter::new("umu-run").glob("umu-run"),
            FileFilter::new("все файлы").glob("*"),
        ],
        _ => Vec::new(),
    }
}

/// Открыть системный диалог и дождаться ответа. Ok(None) — путь не выбран,
/// это не ошибка и в поле ничего не идёт.
async fn pick_path(target: PickTarget, start: PathBuf) -> Result<Option<PathBuf>, String> {
    let request = SelectedFiles::open_file()
        .title(target.title())
        .directory(!target.wants_file())
        .filters(pick_filters(target))
        .current_folder(&start)
        .map_err(|e| format!("не показать каталог {}: {e}", start.display()))?;
    // send() сам ждёт сигнал Response: диалог открыт, ответ получен — остаётся
    // только разобрать, что на нём прислали.
    let answer = match request.send().await {
        Ok(sent) => sent.response(),
        Err(e) => Err(e),
    };
    let files = match answer {
        Ok(files) => files,
        Err(e) if is_declined(&e) => return Ok(None),
        Err(e) => return Err(portal_error(e)),
    };
    Ok(files.uris().first().and_then(|u| uri_to_path(u.as_str())))
}

/// Путь не выбран: отмена, закрытое окно диалога или «другая ошибка» фронтенда,
/// которая у gtk/kde означает именно то, что человек ушёл из диалога, ничего не
/// выбрав. Ошибкой это быть не должно — иначе красное окно за обычное Esc.
fn is_declined(e: &ashpd::Error) -> bool {
    matches!(
        e,
        ashpd::Error::Response(ResponseError::Cancelled | ResponseError::Other)
            | ashpd::Error::Portal(PortalError::Cancelled(_) | PortalError::WindowDestroyed(_))
    )
}

/// Что сказать, когда диалог не открылся. Обычно это отсутствие портала, и
/// именно его человек ищет в списке пакетов.
fn portal_error(e: ashpd::Error) -> String {
    match &e {
        ashpd::Error::PortalNotFound(name) => format!(
            "системного диалога нет: интерфейс {name} ни одному сервису не \
             принадлежит. нужны запущенный xdg-desktop-portal и его frontend \
             (gtk, kde или hyprland)"
        ),
        _ => format!("системный диалог не открылся: {e}"),
    }
}

/// Путь из file:// URI, которую вернул портал: схема, затем абсолютный путь,
/// пробелы и не-ASCII — в percent-encoding.
fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    // «file:///home/…» — пустой хост, путь начинается сразу после схемы. Хост в
    // file:// — это чужая машина, такого лаунчер не откроет.
    if !path.starts_with('/') {
        return None;
    }
    percent_decode(path).map(PathBuf::from)
}

/// Раскодирование percent-encoding в UTF-8: байты после %XX, целыми группами.
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = char::from(bytes[i + 1]).to_digit(16)?;
            let lo = char::from(bytes[i + 2]).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote = None::<char>;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_env(s: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for pair in s.split([';', '\n']) {
        if let Some((k, v)) = pair.split_once('=')
            && !k.trim().is_empty()
        {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

fn load() -> Result<(Config, PathBuf), String> {
    let path = config_path();
    let config = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("не смог прочитать {}: {e}", path.display()))?;
        toml::from_str::<Config>(&raw)
            .map_err(|e| format!("битый TOML в {}: {e}", path.display()))?
    } else {
        Config::default()
    };
    let umu = config
        .umu_run
        .as_deref()
        // Пустая строка в конфиге — это «не задано», а не путь-пустота: иначе
        // в команде запуска появляется '' вместо имени обёртки.
        .filter(|s| !s.trim().is_empty())
        .map(expand)
        .unwrap_or_else(which_umu);
    Ok((config, umu))
}

fn which_umu() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    for cand in [
        PathBuf::from("/run/current-system/sw/bin/umu-run"),
        PathBuf::from("/usr/local/bin/umu-run"),
        PathBuf::from("/usr/bin/umu-run"),
        PathBuf::from(format!("{home}/.local/bin/umu-run")),
    ] {
        if cand.exists() {
            return cand;
        }
    }
    for entry in std::env::var("PATH").unwrap_or_default().split(':') {
        let cand = PathBuf::from(entry).join("umu-run");
        if cand.exists() {
            return cand;
        }
    }
    PathBuf::from("umu-run")
}

/// Каталог из поиска раннеров. `game_dir` — папка с установленными играми
/// (`<библиотека>/steamapps/common`): там рядом с Proton лежат сотни гигабайт
/// чужих файлов, поэтому заходим только по каталогам с «proton» в имени и не
/// ныряем на уровень глубже.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchDir {
    path: PathBuf,
    game_dir: bool,
}

/// Куда заглядываем по умолчанию. Отдельно от фильтра по существующим каталогам,
/// чтобы список мест можно было проверить тестом, а не «повезёт — не повезёт».
fn runner_search_candidates(extra: &[String]) -> Vec<SearchDir> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let data = dirs::data_dir().unwrap_or_else(|| home.join(".local/share"));
    let plain = |path: PathBuf| SearchDir {
        path,
        game_dir: false,
    };
    let mut dirs: Vec<SearchDir> = extra.iter().map(|d| plain(expand(d))).collect::<Vec<_>>();
    dirs.extend([
        plain(proton_install_dir()),
        plain(home.join(".steam/steam/compatibilitytools.d")),
        plain(home.join(".steam/debian-installation/compatibilitytools.d")),
        plain(data.join("lutris/runners/proton")),
        plain(data.join("heroic/tools/proton")),
        plain(data.join("umu")),
        plain(PathBuf::from("/opt/umu/runners")),
        // Сюда umu-launcher сам скачивает GE-Proton, когда игре не задали
        // PROTONPATH: без этого раннер есть на диске, но в списке его нет.
        plain(
            dirs::cache_dir()
                .unwrap_or_else(|| home.join(".cache"))
                .join("umu"),
        ),
        plain(home.join(".cache/umu")),
        // Steam и Lutris из flatpak держат всё внутри ~/.var — там лежат
        // раннеры, поставленные ProtonUp-Qt.
        plain(home.join(".var/app/com.valvesoftware.Steam/data/Steam/compatibilitytools.d")),
        plain(
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/compatibilitytools.d"),
        ),
        plain(home.join(".var/app/net.lutris.Lutris/data/lutris/runners/proton")),
        // PortProton держит свои сборки рядом с приложением; у flatpak-версии
        // отдельный корень внутри ~/.var.
        plain(data.join("PortProton")),
        plain(data.join("portproton")),
        plain(home.join(".cache/portproton")),
        plain(PathBuf::from("/opt/PortProton")),
        plain(home.join(".var/app/ru.linux_gaming.PortProton/data/PortProton")),
    ]);
    // Нативный Proton Steam («Proton 10.0», «Proton - Experimental») лежит не в
    // compatibilitytools.d, а в steamapps/common каждой библиотеки.
    for lib in steam_libraries() {
        dirs.push(SearchDir {
            path: lib.join("steamapps/common"),
            game_dir: true,
        });
    }
    dirs
}

/// То же, но только существующие каталоги: примонтированный второй диск или
/// flatpak-Steam добавляются, когда они реально есть.
fn runner_search_dirs(extra: &[String]) -> Vec<SearchDir> {
    let mut dirs = runner_search_candidates(extra);
    dirs.retain(|d| d.path.is_dir());
    dirs
}

/// Библиотеки Steam: пути из libraryfolders.vdf плюс типовые, которых в vdf
/// бывает не записано.
fn steam_libraries() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let mut libs: Vec<PathBuf> = Vec::new();
    for vdf in [
        home.join(".local/share/Steam/steamapps/libraryfolders.vdf"),
        home.join(".steam/steam/steamapps/libraryfolders.vdf"),
        home.join(
            ".var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/libraryfolders.vdf",
        ),
    ] {
        let Ok(raw) = std::fs::read_to_string(vdf) else {
            continue;
        };
        for p in library_paths_from_vdf(&raw) {
            let p = PathBuf::from(p);
            if !libs.contains(&p) {
                libs.push(p);
            }
        }
    }
    for cand in [
        home.join(".local/share/Steam"),
        home.join("SteamLibrary"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
    ] {
        if !libs.contains(&cand) {
            libs.push(cand);
        }
    }
    libs
}

/// Из «libraryfolders.vdf» нужны только значения «"path" "…"»: формат Valve —
/// по одной паре ключ/значение на строку, остальное нам не интересно.
fn library_paths_from_vdf(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let Some(rest) = line.trim().strip_prefix("\"path\"") else {
            continue;
        };
        let Some(value) = rest
            .trim()
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
        else {
            continue;
        };
        if !value.trim().is_empty() {
            out.push(value.trim().to_string());
        }
    }
    out
}

/// Похож ли каталог библиотеки на раннер по имени: в steamapps/common иначе
/// перебираем все установленные игры ради одной папки Proton.
fn proton_in_name(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_lowercase().contains("proton"))
}

/// Куда ставим скачанные раннеры: тот же каталог Steam, что уже просматривает
/// поиск раннеров, — новый GE-Proton появляется в списках выбора сам.
fn proton_install_dir() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    dirs::data_dir()
        .unwrap_or_else(|| home.join(".local/share"))
        .join("Steam/compatibilitytools.d")
}

/// Имена каталогов раннеров в папке установки (для отметки «уже установлен»).
fn installed_runners(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| runner_at(&e.path()).is_some())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();
    names.sort();
    names.reverse();
    names
}

/// Каталог раннера может называться не тегом, а тегом с суффиксом архитектуры:
/// архив GE-Proton раскладывается в `GE-Proton11-11-x86_64`. Граница имени
/// обязательна, иначе `GE-Proton11-1` посчитал бы установленным `GE-Proton11-11`.
fn installed_as<'a>(names: &'a [String], tag: &str) -> Option<&'a str> {
    let prefix = format!("{tag}-");
    let legacy = format!("Proton-{tag}");
    names
        .iter()
        .map(String::as_str)
        .find(|n| *n == tag || n.starts_with(prefix.as_str()) || *n == legacy)
}

fn discover_runners(extra: &[String]) -> Vec<Runner> {
    let mut found = Vec::new();
    for dir in runner_search_dirs(extra) {
        let Ok(entries) = std::fs::read_dir(&dir.path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if dir.game_dir && !proton_in_name(&p) {
                continue;
            }
            if let Some(r) = runner_at(&p) {
                found.push(r);
            } else if p.is_dir() && !dir.game_dir {
                // иногда раннер распакован на уровень глубже: <dir>/<name>/proton
                let Ok(inner) = std::fs::read_dir(&p) else {
                    continue;
                };
                for e2 in inner.flatten() {
                    if let Some(r) = runner_at(&e2.path()) {
                        found.push(r);
                    }
                }
            }
        }
    }
    let mut deduped: Vec<Runner> = Vec::new();
    for r in found {
        let Some(real) = r.canonical() else { continue };
        if !deduped.iter().any(|e| e.same(real.clone())) {
            deduped.push(r);
        }
    }
    deduped.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out = vec![Runner { path: None }];
    out.extend(deduped);
    out
}

fn runner_at(p: &Path) -> Option<Runner> {
    if p.is_dir() && p.join("proton").is_file() {
        return Some(Runner {
            path: Some(p.to_string_lossy().into_owned()),
        });
    }
    if p.is_file() && p.file_name().is_some_and(|n| n == "proton") {
        return p.parent().map(|d| Runner {
            path: Some(d.to_string_lossy().into_owned()),
        });
    }
    None
}

fn tail(path: &Path, bytes: u64) -> String {
    let Ok(meta) = path.metadata() else {
        return String::from("лога нет");
    };
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::from("лог не читается");
    };
    let start = meta.len().saturating_sub(bytes);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut buf = Vec::new();
    if f.read_to_end(&mut buf).is_err() {
        return String::new();
    }
    if start > 0
        && let Some(pos) = buf.iter().position(|b| *b == b'\n')
    {
        buf.drain(..=pos);
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(150)..].join("\n")
}

// ---------------------------------------------------------------- launching

fn resolve(
    cfg: &GameCfg,
    defaults: &Defaults,
) -> Result<(PathBuf, PathBuf, String, String, Option<String>), String> {
    let exe = expand(cfg.exe.trim());
    if !exe.is_file() {
        return Err(format!("нет файла: {}", exe.display()));
    }
    let gameid = cfg
        .gameid
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| slug(&cfg.name));
    let store = cfg
        .store
        .clone()
        .or(defaults.store.clone())
        .unwrap_or_else(|| "none".into());
    let prefix = match cfg.prefix.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(p) => expand(p),
        None => expand(defaults.prefix_root.as_deref().unwrap_or("~/Games/umu")).join(&gameid),
    };
    let runner = cfg
        .runner
        .clone()
        .or(defaults.runner.clone())
        .filter(|s| !s.trim().is_empty());
    Ok((exe, prefix, gameid, store, runner))
}

/// Печатаем команду так, чтобы её можно было вставить в терминал: пути с
/// пробелами и кавычками экранируем, обычные — оставляем как есть.
fn shq(s: &str) -> String {
    if !s.is_empty()
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '-' | '/' | ':' | '=' | '@' | '%' | '+' | ',')
        })
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn build_command(
    cfg: &GameCfg,
    umu: &Path,
    defaults: &Defaults,
) -> Result<(Command, String), String> {
    let (exe, prefix, gameid, store, runner) = resolve(cfg, defaults)?;
    let mut cmd = Command::new(umu);
    cmd.arg(&exe).args(&cfg.args);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }

    let mut env = cfg.env.clone();
    env.insert("WINEPREFIX".into(), prefix.display().to_string());
    env.insert("GAMEID".into(), gameid);
    env.insert("STORE".into(), store);
    if let Some(r) = &runner {
        env.insert("PROTONPATH".into(), expand(r).display().to_string());
    }
    env.entry("UMU_LOG".to_string())
        .or_insert_with(|| "1".to_string());

    // Proton заводит игре настоящий буквенный диск только когда переданы
    // STEAM_COMPAT_* — это делает Steam, но не каждый сторонний лаунчер.
    if let Some(dir) = exe.parent() {
        let mut cur = Some(dir.to_path_buf());
        while let Some(d) = cur {
            if d.file_name().is_some_and(|n| n == "steamapps") {
                if let Some(lib) = d.parent() {
                    env.insert(
                        "STEAM_COMPAT_INSTALL_PATH".into(),
                        dir.display().to_string(),
                    );
                    env.insert(
                        "STEAM_COMPAT_LIBRARY_PATHS".into(),
                        lib.display().to_string(),
                    );
                }
                break;
            }
            cur = d.parent().map(Path::to_path_buf);
        }
    }

    for (k, v) in &env {
        cmd.env(k, v);
    }

    let pairs = env
        .iter()
        .map(|(k, v)| format!("{k}={}", shq(v)))
        .collect::<Vec<_>>()
        .join(" ");
    let args = cfg
        .args
        .iter()
        .map(|a| shq(a))
        .collect::<Vec<_>>()
        .join(" ");
    let args = if args.is_empty() {
        String::new()
    } else {
        format!(" {args}")
    };
    // Одна строка, которую можно вставить в терминал: cd в каталог игры, переменные
    // окружения, затем umu-run с путём к .exe.
    let desc = format!(
        "cd {} && {} {} {}{}",
        shq(&exe
            .parent()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| ".".into())),
        pairs,
        shq(&umu.display().to_string()),
        shq(&exe.display().to_string()),
        args,
    );
    Ok((cmd, desc))
}

fn plan(cfg: &GameCfg, umu: &Path, defaults: &Defaults) -> Result<String, String> {
    build_command(cfg, umu, defaults).map(|(_, desc)| desc)
}

/// umu-run → proton → wineserver/wine: `kill` только прямому ребёнку оставляет
/// игру висеть в фоне. Собираем всё дерево и гасим от листьев к корню.
/// SIGTERM, а не SIGKILL: wineserver должен успеть записать реестр префикса.
fn kill_tree(pid: u32) -> usize {
    let mut stack = vec![pid];
    let mut order: Vec<u32> = Vec::new();
    while let Some(p) = stack.pop() {
        if order.len() > 512 {
            break;
        }
        order.push(p);
        stack.extend(children_of(p));
    }
    let me = std::process::id();
    let mut killed = 0;
    for p in order.iter().rev().filter(|p| **p != me) {
        let rc = unsafe { libc::kill(*p as libc::pid_t, libc::SIGTERM) };
        if rc == 0 {
            killed += 1;
        }
    }
    killed
}

fn children_of(pid: u32) -> Vec<u32> {
    if let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
        return s
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
    }
    // файла может не быть без CONFIG_PROC_CHILDREN — тогда сканируем /proc по ppid
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(candidate) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if ppid_of(candidate) == Some(pid) {
            out.push(candidate);
        }
    }
    out
}

fn ppid_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `<pid> (имя с пробелами) S <ppid> …` — отсчитываем от последней скобки
    let rest = &stat[(stat.rfind(')')? + 1)..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

fn launch(game: &Game, umu: &Path, defaults: &Defaults) -> Result<Child, String> {
    if !umu.exists() {
        return Err(format!(
            "umu-run не найден: {} — поставьте umu-launcher или пропишите umu_run в конфиге",
            umu.display()
        ));
    }
    let (mut cmd, _) = build_command(&game.cfg, umu, defaults)?;
    if let Ok((_, prefix, ..)) = resolve(&game.cfg, defaults) {
        std::fs::create_dir_all(&prefix)
            .map_err(|e| format!("префикс {}: {e}", prefix.display()))?;
    }
    let open = |log: &PathBuf| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| format!("лог {}: {e}", log.display()))
    };
    let out = open(&game.log)?;
    let err = open(&game.log)?;
    cmd.stdout(Stdio::from(out)).stderr(Stdio::from(err));
    cmd.spawn()
        .map_err(|e| format!("не смог запустить {}: {e}", umu.display()))
}

// ---------------------------------------------------------------- library scan

fn scan_library(root: &Path) -> Vec<GameCfg> {
    let mut libs = Vec::new();
    collect_steamapps(root, 0, &mut libs);
    let mut found = Vec::new();
    for lib in libs {
        let Ok(games) = std::fs::read_dir(&lib) else {
            continue;
        };
        for entry in games.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(exe) = pick_exe(&dir) else { continue };
            found.push(GameCfg {
                name: dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| exe.display().to_string()),
                exe: exe.to_string_lossy().into_owned(),
                ..Default::default()
            });
            if found.len() >= 200 {
                return found;
            }
        }
    }
    found
}

fn collect_steamapps(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 4 || !dir.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if p.file_name().is_some_and(|n| n == "steamapps") && p.join("common").is_dir() {
            out.push(p.join("common"));
        } else {
            collect_steamapps(&p, depth + 1, out);
        }
    }
}

fn pick_exe(game_dir: &Path) -> Option<PathBuf> {
    let mut exes = Vec::new();
    let Ok(entries) = std::fs::read_dir(game_dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
            exes.push(p);
        }
    }
    exes.sort();
    let first = exes.first()?.clone();
    let Some(stem) = game_dir.file_name() else {
        return Some(first);
    };
    let stem = stem.to_string_lossy().to_lowercase();
    let preferred = exes.iter().find(|p| {
        p.file_stem()
            .is_some_and(|s| stem.contains(&s.to_string_lossy().to_lowercase()))
    });
    Some(preferred.cloned().unwrap_or(first))
}

// ---------------------------------------------------------------- proton downloads
//
// umu-launcher умеет скачивать только последнюю сборку, поэтому список версий,
// загрузка и проверка контрольной суммы — здесь. Каталог установки совпадает с
// тем, что и так просматривает поиск раннеров, так что установленный GE-Proton
// сразу появляется в списках выбора и в `PROTONPATH` при запуске.

const GE_RELEASES: &str = "https://api.github.com/repos/GloriousEggroll/proton-ge-custom/releases";
const UA: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
/// Ответ API страницами: 100 релизов за запрос, не больше пяти запросов.
const PAGE: u32 = 100;
const PAGES: u32 = 5;
const CHUNK: usize = 64 * 1024;

/// Короткий ответ: что произошло и почему.
fn http_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(403, _) | ureq::Error::Status(429, _) => {
            "GitHub ограничил обращения (лимит без токена — 60 в час): попробуйте позже".to_string()
        }
        ureq::Error::Status(code, _) => format!("GitHub ответил {code}"),
        ureq::Error::Transport(t) => format!("нет доступа к GitHub: {t}"),
    }
}

fn api_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(UA)
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(20))
        .timeout(Duration::from_secs(60))
        .build()
}

/// Для архивов: таймаута на весь запрос нет — сотни мегабайт читаются минутами,
/// но зависший поток на 30 секундах обрывается.
fn dl_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent(UA)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build()
}

/// Имя файла контрольной суммы говорит, каким хешем сверять; `.sum` без цифр —
/// конвенция umu-launcher, там sha256.
fn algo_of(sum_name: &str) -> Option<Algo> {
    if sum_name.ends_with(".sha512sum") {
        Some(Algo::Sha512)
    } else if sum_name.ends_with(".sha256sum") || sum_name.ends_with(".sum") {
        Some(Algo::Sha256)
    } else {
        None
    }
}

/// Тег обязан быть годным именем каталога: он попадает в путь установки.
fn safe_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag != "."
        && tag != ".."
        && !tag.contains("..")
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Путь из архива не должен выбираться из папки распаковки.
fn escapes_dir(path: &Path) -> bool {
    path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir))
}

/// Цель символьной ссылки внутри архива: `../…` допустимы, пока не уводят за
/// пределы распаковки (в Proton такие ссылки встречаются).
fn escapes_link(entry: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return true;
    }
    let mut depth: i32 = 0;
    for part in entry
        .parent()
        .unwrap_or(Path::new(""))
        .components()
        .chain(target.components())
    {
        match part {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Подходящий ли это релиз для данной архитектуры.
fn matches_arch(name: &str, ours: &str, foreign: &[&str]) -> bool {
    let hits_ours = name.contains(ours) || (ours == "x86_64" && name.contains("amd64"));
    let hits_foreign = foreign.iter().any(|t| name.contains(t));
    if ours == "x86_64" {
        // старые релизы вообще не указывали архитектуру в имени
        hits_ours || (!hits_foreign && !name.contains("arm"))
    } else {
        hits_ours
    }
}

fn arch_tokens() -> (String, &'static [&'static str]) {
    match std::env::consts::ARCH {
        "aarch64" => ("aarch64".to_string(), &["x86_64", "amd64", "i686"][..]),
        other => (other.to_string(), &["aarch64", "arm64"][..]),
    }
}

/// Разбирает ответ API в установки-кандидаты. Релиз без архива для нашей
/// архитектуры просто пропускается: предложить его — значит пообещать то, чего
/// в релизе нет.
fn releases_from(items: &[serde_json::Value]) -> Vec<Release> {
    let (ours, foreign) = arch_tokens();
    releases_for_arch(items, &ours, foreign)
}

fn releases_for_arch(items: &[serde_json::Value], ours: &str, foreign: &[&str]) -> Vec<Release> {
    let mut out = Vec::new();
    for item in items {
        let Some(tag) = item.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        if !safe_tag(tag) || item.get("draft").and_then(|v| v.as_bool()) == Some(true) {
            continue;
        }
        let assets: Vec<(&str, &str, u64)> = item
            .get("assets")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        let name = a.get("name").and_then(|v| v.as_str())?;
                        let url = a.get("browser_download_url").and_then(|v| v.as_str())?;
                        let size = a.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                        Some((name, url, size))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let Some((name, url, size)) = assets
            .iter()
            .find(|(n, _, _)| n.ends_with(".tar.gz") && matches_arch(n, ours, foreign))
        else {
            continue;
        };
        let stem = name.trim_end_matches(".tar.gz");
        let sum = assets
            .iter()
            .find(|(n, _, _)| n.starts_with(stem) && algo_of(n).is_some())
            .and_then(|(n, u, _)| algo_of(n).map(|a| (u.to_string(), a)));
        let date = item
            .get("published_at")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .split('T')
            .next()
            .unwrap_or_default()
            .to_string();
        out.push(Release {
            tag: tag.to_string(),
            url: url.to_string(),
            sum: sum.map(|(u, a)| (u.to_string(), a)),
            size: *size,
            date,
        });
    }
    out
}

fn list_releases() -> Result<Vec<Release>, String> {
    let agent = api_agent();
    let mut out = Vec::new();
    for page in 1..=PAGES {
        let url = format!("{GE_RELEASES}?per_page={PAGE}&page={page}");
        let body = agent
            .get(&url)
            .call()
            .map_err(http_err)?
            .into_string()
            .map_err(|e| format!("не смог прочитать ответ GitHub: {e}"))?;
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&body).map_err(|e| format!("непонятный JSON от GitHub: {e}"))?;
        let count = items.len();
        out.extend(releases_from(&items));
        if (count as u32) < PAGE {
            break;
        }
    }
    if out.is_empty() {
        return Err("GE-Proton не найден в ответе GitHub".to_string());
    }
    Ok(out)
}

async fn list_task() -> Result<Vec<Release>, String> {
    tokio::task::spawn_blocking(list_releases)
        .await
        .map_err(|e| format!("запрос релизов прерван: {e}"))?
}

enum Digester {
    Sha512(sha2::Sha512),
    Sha256(sha2::Sha256),
}

impl Digester {
    fn new(algo: Algo) -> Self {
        use sha2::Digest;
        match algo {
            Algo::Sha512 => Digester::Sha512(sha2::Sha512::new()),
            Algo::Sha256 => Digester::Sha256(sha2::Sha256::new()),
        }
    }

    fn update(&mut self, buf: &[u8]) {
        use sha2::Digest;
        match self {
            Digester::Sha512(d) => d.update(buf),
            Digester::Sha256(d) => d.update(buf),
        }
    }

    fn hex(self) -> String {
        use sha2::Digest;
        let bytes = match self {
            Digester::Sha512(d) => d.finalize().to_vec(),
            Digester::Sha256(d) => d.finalize().to_vec(),
        };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Скачивает архив и, если известен алгоритм, считает хеш по пути — так не
/// нужно перечитывать сотни мегабайт второй раз.
fn download(
    url: &str,
    dest: &Path,
    algo: Option<Algo>,
    prog: &DlProgress,
) -> Result<String, String> {
    let response = dl_agent()
        .get(url)
        .call()
        .map_err(|e| format!("не смог скачать архив: {}", http_err(e)))?;
    if let Some(len) = response
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
    {
        prog.set_total(len);
    }
    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("не смог создать {}: {e}", dest.display()))?;
    let mut digester = algo.map(Digester::new);
    let mut buf = vec![0u8; CHUNK];
    let mut reader = response.into_reader();
    loop {
        if prog.cancelled() {
            return Err("установка прервана".to_string());
        }
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("скачивание оборвалось: {e}"))?;
        if n == 0 {
            break;
        }
        if let Some(d) = digester.as_mut() {
            d.update(&buf[..n]);
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("не смог записать {}: {e}", dest.display()))?;
        prog.add(n as u64);
    }
    file.flush()
        .map_err(|e| format!("не смог дописать {}: {e}", dest.display()))?;
    Ok(digester.map(|d| d.hex()).unwrap_or_default())
}

/// Ждём `<хеш>  <имя файла>`; имя не проверяем — релизы переименовывают.
fn expected_sum(url: &str, algo: Algo) -> Result<String, String> {
    let body = api_agent()
        .get(url)
        .call()
        .map_err(|e| format!("не смог скачать контрольную сумму: {}", http_err(e)))?
        .into_string()
        .map_err(|e| format!("не смог прочитать контрольную сумму: {e}"))?;
    parse_sum(&body, algo)
}

/// Формат файла — `<хеш>  <имя>`; имя не проверяем, релизы переименовывают.
fn parse_sum(body: &str, algo: Algo) -> Result<String, String> {
    let hex = body
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if hex.len() != algo.hex_len() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "файл контрольной суммы не похож на {}: ожидалось {} символов",
            algo.label(),
            algo.hex_len()
        ));
    }
    Ok(hex)
}

fn unpack(archive: &Path, dest: &Path, prog: &DlProgress) -> Result<(), String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("не смог открыть {}: {e}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(std::io::BufReader::new(file));
    let mut ar = tar::Archive::new(gz);
    let entries = ar.entries().map_err(|e| format!("битый архив: {e}"))?;
    for entry in entries {
        if prog.cancelled() {
            return Err("установка прервана".to_string());
        }
        let mut entry = entry.map_err(|e| format!("битый архив: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("имя файла в архиве не читается: {e}"))?
            .into_owned();
        if escapes_dir(&path) {
            return Err(format!(
                "архив содержит путь вне папки установки: {}",
                path.display()
            ));
        }
        if let Ok(Some(link)) = entry.link_name()
            && escapes_link(&path, Path::new(link.as_ref()))
        {
            return Err(format!(
                "архив содержит ссылку вне папки установки: {} -> {}",
                path.display(),
                link.display()
            ));
        }
        let full = dest.join(&path);
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&full)
                .map_err(|e| format!("не смог создать {}: {e}", full.display()))?;
        } else {
            // архив может не содержать записи о каталогах — создаём их сами
            if let Some(parent) = full.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                return Err(format!("не смог создать {}: {e}", parent.display()));
            }
            entry
                .unpack(&full)
                .map_err(|e| format!("не смог распаковать {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Как уложен архив: обычно один каталог с именем тега, но если записей
/// несколько или `proton` не на уровне выше — собираем каталог по тегу сами.
enum Layout {
    /// Один верхний каталог — переносим его как есть.
    Single(String),
    /// Содержимое раскладываем в `<папка>/<тег>`.
    Bundle,
}

fn layout(unpacked: &Path) -> Layout {
    let Ok(entries) = std::fs::read_dir(unpacked) else {
        return Layout::Bundle;
    };
    let names: Vec<std::ffi::OsString> = entries.flatten().map(|e| e.file_name()).collect();
    if names.len() != 1 {
        return Layout::Bundle;
    }
    let dir = unpacked.join(&names[0]);
    if dir.is_dir() && dir.join("proton").is_file() {
        Layout::Single(names[0].to_string_lossy().into_owned())
    } else {
        Layout::Bundle
    }
}

fn install_into(tmp: &Path, rel: &Release, prog: &DlProgress) -> Result<PathBuf, String> {
    std::fs::create_dir_all(tmp).map_err(|e| format!("не смог создать {}: {e}", tmp.display()))?;
    let archive = tmp.join("proton.tar.gz");
    let unpacked = tmp.join("unpacked");
    std::fs::create_dir_all(&unpacked)
        .map_err(|e| format!("не смог создать {}: {e}", unpacked.display()))?;
    let algo = rel.sum.as_ref().map(|(_, a)| *a);
    prog.set_stage(Stage::Download);
    let got = download(&rel.url, &archive, algo, prog)?;
    if let Some((url, algo)) = rel.sum.as_ref() {
        prog.set_stage(Stage::Checksum);
        let want = expected_sum(url, *algo)?;
        prog.set_stage(Stage::Verify);
        if got != want {
            return Err(format!(
                "{} не совпал: скачано {got}, в релизе {want}",
                algo.label()
            ));
        }
    }
    prog.set_stage(Stage::Unpack);
    unpack(&archive, &unpacked, prog)?;
    if prog.cancelled() {
        return Err("установка прервана".to_string());
    }
    let (name, dest) = match layout(&unpacked) {
        Layout::Single(dir) => (dir.clone(), proton_install_dir().join(dir)),
        Layout::Bundle => (rel.tag.clone(), proton_install_dir().join(&rel.tag)),
    };
    if dest.exists() {
        return Err(format!(
            "{} уже есть — удалите каталог вручную, если хотите переустановить",
            dest.display()
        ));
    }
    move_into(&unpacked, &name, &dest)?;
    if !dest.join("proton").is_file() {
        return Err(format!("в {} нет файла `proton`", dest.display()));
    }
    Ok(dest)
}

/// Единственный верхний каталог переносится целиком, иначе содержимое
/// раскладывается по каталогу с тегом.
fn move_into(unpacked: &Path, name: &str, dest: &Path) -> Result<(), String> {
    let single = unpacked.join(name);
    if single.is_dir() && list_len(unpacked) == 1 {
        return std::fs::rename(&single, dest).map_err(|e| format!("не смог перенести: {e}"));
    }
    std::fs::create_dir_all(dest)
        .map_err(|e| format!("не смог создать {}: {e}", dest.display()))?;
    let Ok(entries) = std::fs::read_dir(unpacked) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        std::fs::rename(entry.path(), dest.join(entry.file_name()))
            .map_err(|e| format!("не смог перенести {}: {e}", entry.path().display()))?;
    }
    Ok(())
}

fn list_len(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|r| r.flatten().count())
        .unwrap_or(usize::MAX)
}

/// Свободное место в байтах; None — не удалось спросить у системы.
fn free_space(path: &Path) -> Option<u64> {
    let cstr = std::ffi::CString::new(path.to_string_lossy().as_ref()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(cstr.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    // f_bavail и f_frsize в разных libc — то u64, то u32: считаем в u128 и
    // сужаемся обратно уже проверенно
    let bytes = stat.f_bavail as u128 * stat.f_frsize as u128;
    u64::try_from(bytes).ok()
}

fn install_release(rel: &Release, prog: &DlProgress) -> Result<PathBuf, String> {
    let target = proton_install_dir();
    if let Some(found) = installed_as(&installed_runners(&target), &rel.tag) {
        return Err(format!("{found} уже установлен"));
    }
    std::fs::create_dir_all(&target)
        .map_err(|e| format!("не смог создать {}: {e}", target.display()))?;
    // архив плюс распакованный раннер: GE-Proton занимает примерно два объёма
    // архива, треть оставляем с запасом
    if let Some(free) = free_space(&target)
        && rel.size > 0
    {
        let need = rel.size.saturating_mul(3);
        if free < need {
            return Err(format!(
                "на диске свободно {}, а нужно около {}",
                human_size(free),
                human_size(need)
            ));
        }
    }
    let tmp = target.join(format!(".umulaunch-{}", rel.tag));
    let _ = std::fs::remove_dir_all(&tmp);
    let result = install_into(&tmp, rel, prog);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

async fn install_task(rel: Release, prog: Arc<DlProgress>) -> Result<PathBuf, String> {
    tokio::task::spawn_blocking(move || install_release(&rel, &prog))
        .await
        .map_err(|e| format!("установка прервана: {e}"))?
}

/// `--proton-list`: что можно поставить, и что уже стоит.
fn proton_list() {
    let dir = proton_install_dir();
    let releases = match list_releases() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!(
        "GE-Proton для {} · установка в {}",
        std::env::consts::ARCH,
        dir.display()
    );
    let installed = installed_runners(&dir);
    for r in &releases {
        let check = r
            .sum
            .as_ref()
            .map(|(_, algo)| algo.label())
            .unwrap_or("без хеша");
        println!(
            "  {:<18} {:>9}  {}  {:<8} {}",
            r.tag,
            human_size(r.size),
            r.date,
            check,
            if installed_as(&installed, &r.tag).is_some() {
                "установлен"
            } else {
                ""
            }
        );
    }
}

/// `--proton-install <ТЕГ>`: то же, что делает панель, но в терминале.
fn proton_install(tag: &str) {
    let releases = match list_releases() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let Some(release) = releases.into_iter().find(|r| r.tag == tag) else {
        eprintln!("нет такой версии: {tag} — посмотрите umulaunch --proton-list");
        std::process::exit(1);
    };
    let progress = Arc::new(DlProgress::default());
    progress.set_total(release.size);
    let done = Arc::new(AtomicBool::new(false));
    let printer = {
        let (progress, done) = (Arc::clone(&progress), Arc::clone(&done));
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let line = match progress.fraction() {
                    Some(part) => format!(
                        "{} · {} из {} ({:.0}%)",
                        progress.stage().label(),
                        human_size(progress.written()),
                        human_size(progress.total().unwrap_or_default()),
                        part * 100.0
                    ),
                    None => format!(
                        "{} · {}",
                        progress.stage().label(),
                        human_size(progress.written())
                    ),
                };
                eprint!("\r{line:<60}");
                let _ = std::io::stderr().flush();
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    };
    let result = install_release(&release, &progress);
    done.store(true, Ordering::Relaxed);
    let _ = printer.join();
    eprintln!();
    match result {
        Ok(path) => println!("установлено: {}", path.display()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// 539 MiB, 1.2 GiB — размеры релизов читают люди, а не компилятор.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} Б");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 10.0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------- update

/// Сообщения панели загрузки Proton: у них есть сетевая часть, поэтому они
/// возвращают задачу. Остальные сообщения — `None`, их смотрит общий match.
fn dl_update(state: &mut State, message: &Message) -> Option<Task<Message>> {
    match message {
        Message::DlToggle => {
            if state.draft.is_some() {
                state.warn = Some(
                    "карточка игры занимает правую панель: закройте её (Esc) и откройте загрузки"
                        .to_string(),
                );
                return None;
            }
            state.dl_open = !state.dl_open;
            state.dl_error = None;
            if state.dl_open && state.dl_releases.is_empty() && state.dl_job.is_none() {
                state.dl_status = DlStatus::Listing;
                return Some(Task::perform(list_task(), Message::DlListed));
            }
            None
        }
        Message::DlList => {
            if state.dl_job.is_some() {
                state.dl_error = Some("сначала дождитесь или прервите установку".to_string());
                return None;
            }
            state.dl_status = DlStatus::Listing;
            state.dl_error = None;
            Some(Task::perform(list_task(), Message::DlListed))
        }
        Message::DlChosen(release) => {
            state.dl_chosen = Some(release.clone());
            state.dl_error = None;
            None
        }
        Message::DlInstall => {
            let Some(release) = state.dl_chosen.clone() else {
                state.dl_error = Some("выберите версию из списка".to_string());
                return None;
            };
            if state.dl_job.is_some() {
                return None;
            }
            if proton_install_dir().join(&release.tag).exists() {
                state.dl_error = Some(format!("{} уже установлен", release.tag));
                return None;
            }
            let progress = Arc::new(DlProgress::default());
            progress.set_total(release.size);
            state.dl_job = Some(progress.clone());
            state.dl_status = DlStatus::Working;
            state.dl_last = None;
            state.dl_error = None;
            state.notice = None;
            state.warn = if release.sum.is_none() {
                Some("в этом релизе нет файла контрольной суммы — ставим как есть".to_string())
            } else {
                None
            };
            Some(Task::perform(
                install_task(release, progress),
                Message::DlDone,
            ))
        }
        Message::DlCancel => {
            if let Some(progress) = &state.dl_job {
                progress.cancel();
                state.dl_error = Some("прерываю: дожидаемся текущего блока".to_string());
            }
            None
        }
        Message::DlListed(result) => {
            state.dl_status = DlStatus::Idle;
            match result {
                Ok(releases) => {
                    state.dl_chosen = releases.first().cloned();
                    state.dl_releases = releases.clone();
                    state.dl_error = None;
                    state.notice = Some(format!(
                        "получено {} версий GE-Proton — самый новый выбран",
                        releases.len()
                    ));
                }
                Err(e) => {
                    state.dl_releases.clear();
                    state.dl_chosen = None;
                    state.dl_error = Some(e.clone());
                }
            }
            None
        }
        Message::DlDone(result) => {
            state.dl_job = None;
            state.dl_status = DlStatus::Idle;
            // каталог установки мог появиться впервые, поэтому списки раннеров
            // пересобираем целиком, а не дополняем
            state.runners = discover_runners(&split_dirs(&state.runner_dirs_input));
            state.dl_installed = installed_runners(&proton_install_dir());
            match result {
                Ok(path) => {
                    state.dl_last = Some(path.clone());
                    state.dl_error = None;
                    state.notice = Some(format!(
                        "{} установлен — он уже в списках раннеров",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.display().to_string())
                    ));
                }
                Err(e) => {
                    state.dl_error = Some(e.clone());
                }
            }
            None
        }
        _ => None,
    }
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    // Действие (кнопка, выбор, сохранение) снимает прошлые подсказки: они
    // относились к прежнему шагу. Печать в полях и тик — нет.
    match message {
        Message::Tick
        | Message::Filter(_)
        | Message::ScanDir(_)
        | Message::PrefixRoot(_)
        | Message::UmuRun(_)
        | Message::RunnerDirs(_)
        | Message::DraftName(_)
        | Message::DraftExe(_)
        | Message::DraftPrefix(_)
        | Message::DraftGameId(_)
        | Message::DraftStore(_)
        | Message::DraftRunner(_)
        | Message::DraftArgs(_)
        | Message::DraftEnv(_)
        | Message::DefaultRunner(_)
        | Message::DefaultStore(_) => {}
        _ => {
            state.notice = None;
            state.warn = None;
        }
    }
    if let Some(task) = dl_update(state, &message) {
        return task;
    }
    match message {
        Message::Reload => state.reload(),
        Message::Save => {
            // Ошибка уже видна в баннере, Result нужен только закрытию окна.
            let _ = state.save();
        }
        Message::Quit => return quit(state),
        Message::New => state.open_draft(None),
        Message::SettingsToggle => state.settings_open = !state.settings_open,
        Message::Pick(target) => {
            // Диалог рисует портал, поэтому он асинхронный: ждём ответ в фоновой
            // задаче, а окно лаунчера остаётся живым.
            let start = start_dir(&state.pick_value(target));
            return Task::perform(pick_path(target, start), move |answer| {
                Message::Picked(target, answer)
            });
        }
        Message::Picked(target, Ok(Some(path))) => {
            state.apply_pick(target, path);
            state.error = None;
        }
        // Отмена диалога — не ошибка: поле остаётся как было.
        Message::Picked(_, Ok(None)) => {}
        Message::Picked(_, Err(e)) => state.error = Some(e),
        Message::Start(i) => {
            let (umu, defaults) = (state.umu.clone(), state.defaults.clone());
            if let Some(game) = state.games.get_mut(i)
                && game.child.is_none()
            {
                state.error = None;
                match launch(game, &umu, &defaults) {
                    Ok(child) => {
                        game.exit = None;
                        game.started = Some(Instant::now());
                        game.child = Some(child);
                        state.selected = Some(i);
                    }
                    Err(e) => state.error = Some(e),
                }
            }
        }
        Message::Stop(i) => {
            if let Some(game) = state.games.get_mut(i)
                && let Some(child) = game.child.as_mut()
            {
                let killed = kill_tree(child.id());
                state.notice = Some(format!("остановлено: завершено процессов — {killed}"));
            }
        }
        Message::Select(i) => {
            state.selected = Some(i);
            state.confirm_delete = None;
        }
        Message::Edit(i) => {
            state.confirm_delete = None;
            state.open_draft(Some(i));
        }
        Message::Delete(i) => {
            if i >= state.games.len() {
                return Task::none();
            }
            if state.games[i].child.is_some() {
                state.error = Some("сначала остановите игру".to_string());
                return Task::none();
            }
            // удаление необратимо для конфига — просим подтвердить вторым нажатием
            if state.confirm_delete != Some(i) {
                state.confirm_delete = Some(i);
                state.warn = Some(format!(
                    "«{}» будет убрана из списка — подтвердите ещё раз",
                    state.games[i].cfg.name
                ));
                return Task::none();
            }
            state.games.remove(i);
            state.selected = if state.games.is_empty() {
                None
            } else {
                Some(0)
            };
            state.confirm_delete = None;
            state.draft = None;
            state.notice = Some("игра убрана из списка — нажмите «сохранить»".to_string());
        }
        Message::Tick => {
            for game in &mut state.games {
                let Some(child) = game.child.as_mut() else {
                    continue;
                };
                if let Ok(Some(status)) = child.try_wait() {
                    game.exit = Some(match status.code() {
                        Some(0) => "вышла нормально".to_string(),
                        Some(c) => format!("код выхода {c}"),
                        None => "убита сигналом".to_string(),
                    });
                    game.child = None;
                    game.started = None;
                }
            }
        }
        Message::Filter(s) => state.filter = s,
        Message::ScanDir(s) => state.scan_dir = s,
        Message::Scan => {
            let root = expand(state.scan_dir.trim());
            if !root.is_dir() {
                state.error = Some(format!("нет такой папки: {}", root.display()));
                return Task::none();
            }
            let before = state.games.len();
            let mut skipped = 0usize;
            for cfg in scan_library(&root) {
                if state.games.iter().any(|g| {
                    g.cfg.exe == cfg.exe || g.cfg.name.to_lowercase() == cfg.name.to_lowercase()
                }) {
                    skipped += 1;
                    continue;
                }
                let log = log_path(&cfg.name);
                state.games.push(Game {
                    cfg,
                    child: None,
                    started: None,
                    exit: None,
                    log,
                });
            }
            let added = state.games.len() - before;
            state.error = None;
            state.notice = Some(match (added, skipped) {
                (0, 0) => format!(
                    "в {} игр не найдено: нужен корень библиотеки, внутри которого есть steamapps/common",
                    root.display()
                ),
                (0, n) => format!("{n} игр уже в списке, новых не добавилось"),
                (n, 0) => format!("добавлено {n} — нажмите «сохранить», чтобы записать в конфиг"),
                (n, s) => format!("добавлено {n}, повторов пропущено {s}"),
            });
        }
        Message::DefaultRunner(r) => {
            state.defaults.runner = r.path.clone();
            state.ensure_runner(&r.path);
        }
        Message::DefaultStore(s) => state.defaults.store = Some(s),
        Message::PrefixRoot(s) => state.prefix_root_input = s,
        Message::UmuRun(s) => {
            state.umu_input = s;
            let path = expand(&state.umu_input);
            state.umu = if state.umu_input.trim().is_empty() {
                which_umu()
            } else {
                path
            };
        }
        Message::RunnerDirs(s) => state.runner_dirs_input = s,
        Message::DraftName(s) => {
            if let Some(d) = state.draft.as_mut() {
                d.name = s
            }
        }
        Message::DraftExe(s) => {
            if let Some(d) = state.draft.as_mut() {
                d.exe = s
            }
        }
        Message::DraftPrefix(s) => {
            if let Some(d) = state.draft.as_mut() {
                d.prefix = s
            }
        }
        Message::DraftGameId(s) => {
            if let Some(d) = state.draft.as_mut() {
                d.gameid = s
            }
        }
        Message::DraftStore(s) => {
            if let Some(d) = state.draft.as_mut() {
                d.store = s
            }
        }
        Message::DraftRunner(r) => {
            if let Some(d) = state.draft.as_mut() {
                d.runner = r.path.clone();
            }
            state.ensure_runner(&r.path);
        }
        Message::DraftArgs(s) => {
            if let Some(d) = state.draft.as_mut() {
                d.args = s
            }
        }
        Message::DraftEnv(s) => {
            if let Some(d) = state.draft.as_mut() {
                d.env = s
            }
        }
        Message::Commit => state.apply_draft(),
        Message::Cancel => state.draft = None,
        Message::Key(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            use iced::keyboard::Key;
            let cmd = modifiers.contains(Modifiers::COMMAND);
            match key {
                Key::Character(c) if cmd && c.eq_ignore_ascii_case("s") => {
                    let _ = state.save();
                }
                Key::Character(c) if cmd && c.eq_ignore_ascii_case("r") => state.reload(),
                // Ctrl+Q — тот же путь, что и крестик окна: сначала запись, потом выход.
                Key::Character(c) if cmd && c.eq_ignore_ascii_case("q") => return quit(state),
                Key::Named(iced::keyboard::key::Named::Escape) => {
                    // Esc закрывает то, что открыто последним: карточку игры,
                    // потом панель загрузок. Диалог портала закрывается сам.
                    if state.draft.is_some() {
                        state.draft = None;
                    } else {
                        state.dl_open = false;
                    }
                    state.confirm_delete = None;
                }
                _ => {}
            }
        }
        Message::Key(_) => {}
        // загрузкой Proton занимается dl_update: здесь только чтобы match был полным
        Message::DlToggle
        | Message::DlList
        | Message::DlChosen(_)
        | Message::DlInstall
        | Message::DlCancel
        | Message::DlListed(_)
        | Message::DlDone(_) => {}
    }
    Task::none()
}

/// Выход по запросу: сначала пишем конфиг, потом закрываем приложение.
/// Сохраняем до выхода: свёрнутая карточка или забытый Ctrl+S не должны стоить
/// потерянной библиотеки. Ошибку печатаем в терминал — окна уже нет, молча
/// потерять данные хуже.
fn quit(state: &mut State) -> Task<Message> {
    if let Err(e) = state.save() {
        eprintln!("{APP}: {e}");
    }
    iced::exit()
}

/// Запрошено закрыть окно — повод сохранить библиотеку и выйти.
fn on_window_event(
    event: iced::Event,
    _status: iced::event::Status,
    _id: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Window(iced::window::Event::CloseRequested) => Some(Message::Quit),
        _ => None,
    }
}

fn subscription(state: &State) -> Subscription<Message> {
    let keys = keyboard::listen().map(Message::Key);
    let closes = iced::event::listen_with(on_window_event);
    let busy = state.games.iter().any(|g| g.child.is_some()) || state.dl_job.is_some();
    if busy {
        Subscription::batch([
            keys,
            closes,
            iced::time::every(Duration::from_millis(400)).map(|_| Message::Tick),
        ])
    } else {
        Subscription::batch([keys, closes])
    }
}

// ---------------------------------------------------------------- view

/// Тот же цвет, но как мягкая подложка: не нужно заводить новую константу на
/// каждый полупрозрачный оттенок.
fn tint(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

fn soft_shadow(y: f32, blur: f32) -> Shadow {
    Shadow {
        color: tint(Color::BLACK, 0.4),
        offset: Vector { x: 0.0, y },
        blur_radius: blur,
    }
}

fn card_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(CARD)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::from(12.0),
        },
        // Тени нет: программный рендер tiny-skia рисует тень контейнера поверх
        // его же фона, когда внутри есть scrollable, — карточка темнеет пятнами.
        // Разделитель здесь и так делает рамка, а на GPU тень не видна за фоном.
        ..Default::default()
    }
}

fn sunken_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG_DEEP)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::from(9.0),
        },
        ..Default::default()
    }
}

fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(14)
        .width(Length::Fill)
        .style(card_style)
        .into()
}

/// Та же карточка, но тянется по высоте: правая панель должна занимать всё
/// свободное место, иначе scrollable внутри схлопывается в ноль.
fn card_grow<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(14)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(card_style)
        .into()
}

/// Полоса прокрутки рисуется поверх содержимого, поэтому справа оставляем место:
/// иначе кнопка или плашка в последнем ряду оказываются отрезаны.
fn scroll_pad<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(Padding::ZERO.right(12.0))
        .width(Length::Fill)
        .into()
}

fn ghost_style(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(match status {
            button::Status::Hovered | button::Status::Pressed => CARD_HOVER,
            _ => CARD,
        })),
        text_color: TEXT,
        border: Border {
            color: LINE,
            width: 1.0,
            radius: Radius::from(9.0),
        },
        ..Default::default()
    }
}

/// Переключатель панели: когда панель открыта, кнопка выглядит включённой.
fn toggle_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(Background::Color(if active {
            tint(PRIMARY, 0.18)
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            CARD_HOVER
        } else {
            CARD
        })),
        text_color: if active { PRIMARY } else { TEXT },
        border: Border {
            color: if active { tint(PRIMARY, 0.6) } else { LINE },
            width: 1.0,
            radius: Radius::from(9.0),
        },
        ..Default::default()
    }
}

fn primary_style(_: &Theme, status: button::Status) -> button::Style {
    let shade = match status {
        button::Status::Pressed => 0.78,
        button::Status::Hovered => 0.9,
        _ => 1.0,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgb(
            PRIMARY.r * shade,
            PRIMARY.g * shade,
            PRIMARY.b * shade,
        ))),
        text_color: Color::from_rgb(0.08, 0.09, 0.12),
        border: Border {
            color: LINE,
            width: 0.0,
            radius: Radius::from(9.0),
        },
        shadow: soft_shadow(1.0, 6.0),
        ..Default::default()
    }
}

fn danger_style(_: &Theme, status: button::Status) -> button::Style {
    let shade = match status {
        button::Status::Pressed => 0.78,
        button::Status::Hovered => 0.9,
        _ => 1.0,
    };
    button::Style {
        background: Some(Background::Color(Color::from_rgb(
            DANGER.r * shade,
            DANGER.g * shade,
            DANGER.b * shade,
        ))),
        text_color: Color::from_rgb(0.08, 0.09, 0.12),
        border: Border {
            color: LINE,
            width: 0.0,
            radius: Radius::from(9.0),
        },
        ..Default::default()
    }
}

fn row_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: Some(Background::Color(match (selected, status) {
            (_, button::Status::Pressed) => CARD_HOVER.scale_alpha(0.8),
            (true, _) | (_, button::Status::Hovered) => CARD_HOVER,
            _ => CARD,
        })),
        text_color: TEXT,
        border: Border {
            color: if selected { tint(PRIMARY, 0.7) } else { LINE },
            width: 1.0,
            radius: Radius::from(11.0),
        },
        shadow: if selected {
            soft_shadow(2.0, 8.0)
        } else {
            Shadow::default()
        },
        ..Default::default()
    }
}

fn action<'a>(title: &'a str, message: Message) -> Element<'a, Message> {
    button(text(title).size(12))
        .padding([7, 13])
        .style(primary_style)
        .on_press(message)
        .into()
}

fn ghost<'a>(title: &'a str, message: Message) -> Element<'a, Message> {
    button(text(title).size(12))
        .padding([7, 13])
        .style(ghost_style)
        .on_press(message)
        .into()
}

fn danger<'a>(title: &'a str, message: Message) -> Element<'a, Message> {
    button(text(title).size(12))
        .padding([7, 13])
        .style(danger_style)
        .on_press(message)
        .into()
}

fn label(t: impl Into<Cow<'static, str>>) -> Element<'static, Message> {
    text(t.into().into_owned()).size(10).color(MUTED).into()
}

/// Кнопка внутри заголовка раздела: меньше обычной, чтобы не ломать строку.
fn mini<'a>(title: &'a str, message: Message) -> Element<'a, Message> {
    button(text(title).size(11))
        .padding([4, 10])
        .style(ghost_style)
        .on_press(message)
        .into()
}

/// Что действует по умолчанию, одной строкой — так карточку видно, даже
/// когда она свёрнута.
fn settings_summary(state: &State) -> String {
    format!(
        "раннер: {} · store: {} · префиксы: {} · umu-run: {}",
        state.defaults.runner.as_deref().unwrap_or("авто"),
        state.defaults.store.as_deref().unwrap_or("none"),
        state.prefix_root_input,
        umu_label(&state.umu),
    )
}

fn value(t: impl Into<Cow<'static, str>>) -> Element<'static, Message> {
    text(t.into().into_owned())
        .size(12)
        .color(TEXT)
        .wrapping(text::Wrapping::WordOrGlyph)
        .into()
}

/// Подпись слева, значение справа — чтобы карточка читалась как таблица.
fn kv(k: &'static str, v: impl Into<Cow<'static, str>>) -> Row<'static, Message> {
    Row::with_capacity(2)
        .spacing(10)
        .push(text(k).size(10).color(MUTED).width(Length::Fixed(112.0)))
        .push(container(value(v.into().into_owned())).width(Length::Fill))
}

fn labeled<'a>(title: &'static str, child: impl Into<Element<'a, Message>>) -> Column<'a, Message> {
    Column::with_capacity(2)
        .spacing(4)
        .push(label(title))
        .push(child)
}

/// Поле с подписью и пояснением: путь из конфига часто `~/…`, и важно видеть,
/// во что он разворачивается на самом деле.
fn labeled_hint<'a>(
    title: &'static str,
    child: impl Into<Element<'a, Message>>,
    hint: impl Into<Cow<'static, str>>,
) -> Column<'a, Message> {
    Column::with_capacity(3)
        .spacing(4)
        .push(label(title))
        .push(child)
        .push(
            text(hint.into().into_owned())
                .size(10)
                .color(MUTED)
                .wrapping(text::Wrapping::Word),
        )
}

/// Волосатая линия: разделяет заголовок раздела и его содержимое.
fn hairline() -> Element<'static, Message> {
    container(Space::new().width(Length::Fill))
        .height(1)
        .style(|_: &Theme| container::Style {
            background: Some(Background::Color(LINE)),
            ..Default::default()
        })
        .into()
}

/// Заголовок раздела карточки: название слева, справа — сколько чего найдено.
fn section<'a>(title: &'static str, right: Option<Element<'a, Message>>) -> Column<'a, Message> {
    let mut row = Row::with_capacity(2)
        .spacing(10)
        .align_y(Alignment::Center)
        .push(text(title).size(11).color(MUTED));
    if let Some(e) = right {
        row = row.push(e);
    }
    Column::with_capacity(2)
        .spacing(8)
        .push(row)
        .push(hairline())
}

/// Небольшая плашка с именем раннера или статусом.
fn chip(title: impl Into<Cow<'static, str>>, color: Color) -> Element<'static, Message> {
    container(text(title.into().into_owned()).size(10).color(color))
        .padding([3, 9])
        .style(move |_: &Theme| container::Style {
            background: Some(Background::Color(tint(color, 0.10))),
            border: Border {
                color: tint(color, 0.35),
                width: 1.0,
                radius: Radius::from(8.0),
            },
            ..Default::default()
        })
        .into()
}

/// Баннер ответа: то же, что цветной текст, но его невозможно не заметить.
fn banner(
    mark: &'static str,
    caption: &'static str,
    body: impl Into<Cow<'static, str>>,
    color: Color,
) -> Element<'static, Message> {
    let body = body.into().into_owned();
    container(
        Row::with_capacity(3)
            .spacing(10)
            .align_y(Alignment::Center)
            .push(text(mark).size(12).color(color).width(Length::Fixed(14.0)))
            .push(
                text(caption)
                    .size(11)
                    .color(color)
                    .width(Length::Fixed(78.0)),
            )
            .push(
                container(text(body).size(12).color(TEXT).width(Length::Fill)).width(Length::Fill),
            ),
    )
    .padding([9, 12])
    .width(Length::Fill)
    .style(move |_: &Theme| container::Style {
        background: Some(Background::Color(tint(color, 0.10))),
        border: Border {
            color: tint(color, 0.4),
            width: 1.0,
            radius: Radius::from(10.0),
        },
        ..Default::default()
    })
    .into()
}

/// Знак марки приложения: треугольник на плашке акцентного цвета.
fn logotype<'a>() -> Element<'a, Message> {
    container(text("▶").size(11).color(Color::from_rgb(0.08, 0.09, 0.12)))
        // center задаёт и размер, и выравнивание: width/height здесь уже не нужны
        .center(28)
        .style(|_: &Theme| container::Style {
            background: Some(Background::Color(PRIMARY)),
            border: Border {
                color: LINE,
                width: 0.0,
                radius: Radius::from(9.0),
            },
            shadow: soft_shadow(2.0, 8.0),
            ..Default::default()
        })
        .into()
}

fn field<'a>(
    placeholder: &str,
    value: &'a str,
    msg: fn(String) -> Message,
) -> TextInput<'a, Message> {
    text_input(placeholder, value)
        .on_input(msg)
        .size(13)
        .padding([7, 10])
}

/// Поле пути с кнопкой «обзор»: путь из памяти вспоминать не нужно.
fn path_field<'a>(
    placeholder: &'static str,
    value: &'a str,
    msg: fn(String) -> Message,
    target: PickTarget,
) -> Row<'a, Message> {
    Row::with_capacity(2)
        .spacing(6)
        .align_y(Alignment::Center)
        .push(field(placeholder, value, msg).width(Length::Fill))
        .push(mini("обзор", Message::Pick(target)))
}

fn runner_pick<'a>(
    runners: &'a [Runner],
    current: &'a Option<String>,
    msg: fn(Runner) -> Message,
) -> Element<'a, Message> {
    // В конфиге путь мог быть записан как `~/…`, а найденный раннер — как абсолютный:
    // сравниваем по настоящему пути, иначе список показывал бы «авто» при выбранном раннере.
    let selected = match current {
        None => runners
            .iter()
            .find(|r| r.path.is_none())
            .or(runners.first()),
        Some(c) => runners
            .iter()
            .find(|r| r.same(expand(c)))
            .or(runners.first()),
    };
    pick_list(runners, selected, msg)
        .text_size(13)
        .padding([7, 10])
        .width(Length::Fill)
        .into()
}

fn store_pick(current: &str, msg: fn(String) -> Message) -> Element<'static, Message> {
    let selected = STORES.iter().find(|s| **s == current).copied();
    pick_list(STORES.as_slice(), selected, move |s: &'static str| {
        msg(s.to_string())
    })
    .text_size(13)
    .padding([7, 10])
    .into()
}

/// Что имя даст остальным полям, если их не заполнять: GAMEID, префикс, лог.
fn draft_effective(d: &Draft, prefix_root: &str) -> String {
    let name = d.name.trim();
    if name.is_empty() {
        return "имя задаёт GAMEID, каталог префикса и имя файла лога".to_string();
    }
    let id = if d.gameid.trim().is_empty() {
        slug(name)
    } else {
        d.gameid.trim().to_string()
    };
    let prefix = if d.prefix.trim().is_empty() {
        format!("{prefix_root}/{id}")
    } else {
        d.prefix.trim().to_string()
    };
    format!(
        "gameid {id} · префикс {prefix} · лог {}.log",
        log_dir().display()
    )
}

fn umu_tag(umu: &Path) -> String {
    if umu.exists() {
        umu.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| umu.display().to_string())
    } else {
        "umu-run не найден".to_string()
    }
}

/// Путь к umu-run для подписей: если бинарник не найден, показывать
/// относительное «umu-run» бессмысленно — пишем «не найден».
fn umu_label(umu: &Path) -> String {
    if umu.exists() {
        umu.display().to_string()
    } else {
        "не найден".to_string()
    }
}

fn view(state: &State) -> Column<'_, Message> {
    // ---- шапка: марка приложения, чем запускаем, затем действия
    let runner_count = state.runners.len().saturating_sub(1);
    let header = Row::with_capacity(8)
        .spacing(12)
        .align_y(Alignment::Center)
        .push(logotype())
        .push(
            Column::with_capacity(2)
                .spacing(1)
                .push(text(APP).size(18))
                .push(text("игры через umu-launcher").size(10).color(MUTED)),
        )
        .push(chip(
            umu_tag(&state.umu),
            if state.umu.exists() { MUTED } else { WARNING },
        ))
        .push(container(Space::new().width(Length::Fill)))
        .push(
            button(
                text(if state.dl_open {
                    "Proton: закрыть"
                } else {
                    "Proton: загрузки"
                })
                .size(12),
            )
            .padding([7, 13])
            .style(toggle_style(state.dl_open))
            .on_press(Message::DlToggle),
        )
        .push(ghost("перечитать", Message::Reload))
        .push(ghost("сохранить", Message::Save))
        .push(action("новая игра", Message::New));

    // ---- параметры по умолчанию: что уйдёт в окружение, если игра не переопределила
    let settings_body: Element<'_, Message> = if state.settings_open {
        Column::with_capacity(2)
            .spacing(10)
            .push(
                Row::with_capacity(3)
                    .spacing(10)
                    .align_y(Alignment::End)
                    .push(
                        labeled(
                            "раннер по умолчанию",
                            runner_pick(
                                &state.runners,
                                &state.defaults.runner,
                                Message::DefaultRunner,
                            ),
                        )
                        .width(Length::FillPortion(5)),
                    )
                    .push(
                        labeled(
                            "STORE",
                            store_pick(
                                state.defaults.store.as_deref().unwrap_or("none"),
                                Message::DefaultStore,
                            ),
                        )
                        .width(Length::FillPortion(2)),
                    )
                    .push(
                        labeled_hint(
                            "umu-run (пусто — искать в PATH)",
                            path_field(
                                "<umu-run, пусто — искать в PATH>",
                                &state.umu_input,
                                Message::UmuRun,
                                PickTarget::UmuRun,
                            ),
                            umu_label(&state.umu),
                        )
                        .width(Length::FillPortion(5)),
                    ),
            )
            .push(
                Row::with_capacity(2)
                    .spacing(10)
                    .align_y(Alignment::End)
                    .push(
                        labeled_hint(
                            "корень wine-префиксов",
                            path_field(
                                "~/Games/umu",
                                &state.prefix_root_input,
                                Message::PrefixRoot,
                                PickTarget::PrefixRoot,
                            ),
                            format!(
                                "префиксы создаются в {}",
                                expand(&state.prefix_root_input).display()
                            ),
                        )
                        .width(Length::Fill),
                    )
                    .push(
                        labeled_hint(
                            "папки раннеров дополнительно (через ;)",
                            path_field(
                                "~/.local/share/proton-upstream",
                                &state.runner_dirs_input,
                                Message::RunnerDirs,
                                PickTarget::RunnerDirs,
                            ),
                            format!(
                                "дополнительных папок: {}",
                                split_dirs(&state.runner_dirs_input).len()
                            ),
                        )
                        .width(Length::Fill),
                    ),
            )
            .into()
    } else {
        // свёрнутая карточка не должна выглядеть сломанной: показываем, что сейчас действует
        text(settings_summary(state))
            .size(11)
            .color(MUTED)
            .wrapping(text::Wrapping::Word)
            .into()
    };
    let settings = card(
        Column::with_capacity(2)
            .spacing(if state.settings_open { 10 } else { 8 })
            .push(section(
                "по умолчанию",
                Some(
                    Row::with_capacity(2)
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(chip(
                            format!("раннеров найдено {runner_count}"),
                            if runner_count > 0 { SUCCESS } else { WARNING },
                        ))
                        .push(mini(
                            if state.settings_open {
                                "свернуть"
                            } else {
                                "изменить"
                            },
                            Message::SettingsToggle,
                        ))
                        .into(),
                ),
            ))
            .push(settings_body),
    );

    // ---- библиотека
    let mut list = Column::with_capacity(state.games.len() + 1).spacing(7);
    let mut shown = 0usize;
    let filter = state.filter.trim().to_lowercase();
    for (i, g) in state.games.iter().enumerate() {
        if !filter.is_empty()
            && !g.cfg.name.to_lowercase().contains(&filter)
            && !g.cfg.exe.to_lowercase().contains(&filter)
        {
            continue;
        }
        shown += 1;
        list = list.push(game_row(state, i, g));
    }
    if shown == 0 {
        list = list.push(
            container(empty_state(state))
                .padding(18)
                .width(Length::Fill)
                .style(card_style),
        );
    }

    let counted = if state.games.is_empty() {
        "пусто".to_string()
    } else if shown != state.games.len() {
        format!("{shown} из {}", state.games.len())
    } else {
        format!("{} игр", state.games.len())
    };
    let running = state.games.iter().filter(|g| g.child.is_some()).count();

    let mut tools = Row::with_capacity(3)
        .spacing(8)
        .align_y(Alignment::Center)
        .push(
            field(
                "фильтр по имени или пути к .exe",
                &state.filter,
                Message::Filter,
            )
            .width(Length::Fill),
        );
    if !state.filter.is_empty() {
        tools = tools.push(ghost("✕", Message::Filter(String::new())));
    }
    tools = tools.push(chip(counted, MUTED));

    let scan = container(
        Column::with_capacity(2)
            .spacing(5)
            .push(label("папка библиотеки Steam — ищем */steamapps/common/*"))
            .push(
                Row::with_capacity(3)
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        path_field(
                            "~/SteamLibrary",
                            &state.scan_dir,
                            Message::ScanDir,
                            PickTarget::ScanDir,
                        )
                        .width(Length::Fill),
                    )
                    .push(ghost("найти игры", Message::Scan)),
            ),
    )
    .padding(10)
    .width(Length::Fill)
    .style(sunken_style);

    let left = Column::with_capacity(3)
        .spacing(9)
        .width(Length::FillPortion(5))
        .height(Length::Fill)
        .push(tools)
        .push(
            scrollable(scroll_pad(list))
                .height(Length::Fill)
                .width(Length::Fill),
        )
        .push(scan);

    // ---- правая колонка: карточка выбранного, затем лог
    let right_body: Element<'_, Message> = if let Some(d) = &state.draft {
        Column::with_capacity(20)
            .spacing(6)
            .push(label("имя — задаёт имя лога и GAMEID по умолчанию"))
            .push(field("<имя игры>", &d.name, Message::DraftName))
            .push(
                container(
                    text(draft_effective(d, &state.prefix_root_input))
                        .size(10)
                        .color(MUTED)
                        .wrapping(text::Wrapping::Word),
                )
                .padding([4, 8])
                .width(Length::Fill)
                .style(sunken_style),
            )
            .push(label("исполняемый файл .exe (можно ~/)"))
            .push(path_field(
                "<путь к .exe>",
                &d.exe,
                Message::DraftExe,
                PickTarget::DraftExe,
            ))
            .push(label("wine-префикс"))
            .push(path_field(
                "пусто → <корень префиксов>/<GAMEID>",
                &d.prefix,
                Message::DraftPrefix,
                PickTarget::DraftPrefix,
            ))
            .push(label("GAMEID и STORE"))
            .push(
                Row::with_capacity(2)
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        field("<steam-AppID или um-…>", &d.gameid, Message::DraftGameId)
                            .width(Length::Fill),
                    )
                    .push(store_pick(&d.store, Message::DraftStore)),
            )
            .push(label("аргументы"))
            .push(field(
                "<аргументы через пробел>",
                &d.args,
                Message::DraftArgs,
            ))
            // Версию Proton выбираем после флагов запуска: сначала игра пишет,
            // что запускать, и только потом — чем.
            .push(label("раннер для этой игры"))
            .push(runner_pick(&state.runners, &d.runner, Message::DraftRunner))
            .push(label("переменные окружения K=V;K=V"))
            .push(field("PROTON_LOG=1;WINEESYNC=1", &d.env, Message::DraftEnv))
            .push(hairline())
            .push(
                Row::with_capacity(3)
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(action("применить", Message::Commit))
                    .push(ghost("отмена", Message::Cancel))
                    .push(container(Space::new().width(Length::Fill)))
                    .push(delete_button(state)),
            )
            .into()
    } else if state.dl_open {
        dl_body(state)
    } else if let Some(cfg) = state.selected_cfg() {
        let ready = plan(cfg, &state.umu, &state.defaults).is_ok();
        let mut col = Column::with_capacity(14).spacing(7).push(
            Row::with_capacity(2)
                .spacing(10)
                .align_y(Alignment::Center)
                .push(text(cfg.name.clone()).size(18).width(Length::Fill))
                .push(chip(
                    if ready {
                        "готова к запуску"
                    } else {
                        "не готова к запуску"
                    },
                    if ready { SUCCESS } else { DANGER },
                )),
        );
        // Действия — сразу под заголовком: чтобы запустить, карточку не нужно
        // прокручивать, а «удалить» остаётся на другом конце строки.
        if let Some(i) = state.selected {
            let game = &state.games[i];
            col = col.push(
                Row::with_capacity(4)
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(if game.child.is_some() {
                        danger("остановить", Message::Stop(i))
                    } else {
                        action("▶  запустить", Message::Start(i))
                    })
                    .push(ghost("редактировать", Message::Edit(i)))
                    .push(container(Space::new().width(Length::Fill)))
                    .push(delete_button(state)),
            );
        }
        col = col
            .push(kv("исполняемый файл", cfg.exe.clone()))
            .push(kv(
                "раннер",
                match cfg.runner.as_ref().or(state.defaults.runner.as_ref()) {
                    Some(p) => format!(
                        "{}{}",
                        expand(p).display(),
                        if cfg.runner.is_none() {
                            " — по умолчанию"
                        } else {
                            ""
                        }
                    ),
                    None => "авто — решает umu-launcher".into(),
                },
            ))
            .push(kv(
                "wine-префикс",
                match resolve(cfg, &state.defaults) {
                    Ok((_, p, ..)) => p.display().to_string(),
                    // пока .exe не найден, показываем то, что получится по правилам
                    Err(_) => format!(
                        "{}/{}",
                        state.prefix_root_input,
                        cfg.gameid.clone().unwrap_or_else(|| slug(&cfg.name))
                    ),
                },
            ))
            .push(kv(
                "GAMEID · STORE",
                format!(
                    "{} · {}",
                    cfg.gameid.clone().unwrap_or_else(|| slug(&cfg.name)),
                    cfg.store.clone().unwrap_or_else(|| "none".into())
                ),
            ));
        if !cfg.args.is_empty() {
            col = col.push(kv("аргументы", cfg.args.join(" ")));
        }
        if !cfg.env.is_empty() {
            col = col.push(kv(
                "окружение",
                cfg.env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
        col = col.push(
            Column::with_capacity(2)
                .spacing(4)
                .push(label(
                    "команда запуска — та же строка, что печатает --dry-run",
                ))
                .push(
                    container(
                        text(match plan(cfg, &state.umu, &state.defaults) {
                            Ok(desc) => desc,
                            Err(e) => format!("не готово к запуску: {e}"),
                        })
                        .size(11)
                        .color(TEXT)
                        .wrapping(text::Wrapping::WordOrGlyph),
                    )
                    .padding(10)
                    .width(Length::Fill)
                    .style(sunken_style),
                ),
        );
        col.into()
    } else {
        pane_empty(state)
    };

    let pane_title = if state.draft.is_some() {
        "карточка игры"
    } else if state.dl_open {
        "загрузка Proton"
    } else if state.selected_cfg().is_some() {
        "выбранная игра"
    } else {
        "игра не выбрана"
    };

    let log = state.log_text();
    let log_file = state
        .selected
        .and_then(|i| state.games.get(i))
        .map(|g| g.log.display().to_string())
        .unwrap_or_else(|| "игра не выбрана".into());
    let right = Column::with_capacity(2)
        .spacing(9)
        .width(Length::FillPortion(6))
        .height(Length::Fill)
        .push(card_grow(
            Column::with_capacity(2)
                .spacing(10)
                .height(Length::Fill)
                .push(section(pane_title, None))
                .push(
                    scrollable(scroll_pad(right_body))
                        .height(Length::Fill)
                        .width(Length::Fill),
                ),
        ))
        .push(
            Column::with_capacity(2)
                .spacing(5)
                // лог знает свою высоту, остальное достаётся карточке игры
                .height(Length::Fixed(170.0))
                .push(
                    Row::with_capacity(3)
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(label("лог"))
                        .push(
                            container(text(log_file).size(10).color(MUTED).width(Length::Fill))
                                .width(Length::Fill),
                        ),
                )
                .push(
                    container(
                        scrollable(scroll_pad(
                            text(log)
                                .size(11)
                                .color(TEXT)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        ))
                        .height(Length::Fill)
                        .width(Length::Fill)
                        .anchor_bottom()
                        .auto_scroll(true),
                    )
                    .padding(10)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(sunken_style),
                ),
        );

    let mut root = Column::with_capacity(5)
        .spacing(12)
        .push(header)
        .push(settings)
        .push(
            Row::with_capacity(2)
                .spacing(12)
                .push(left)
                .push(right)
                .height(Length::Fill),
        );
    if let Some(e) = &state.error {
        root = root.push(banner("✕", "ошибка", e.clone(), DANGER));
    }
    if let Some(w) = &state.warn {
        root = root.push(banner("!", "внимание", w.clone(), WARNING));
    }
    if let Some(n) = &state.notice {
        root = root.push(banner("✓", "готово", n.clone(), SUCCESS));
    }
    root = root.push(
        Row::with_capacity(7)
            .spacing(8)
            .align_y(Alignment::Center)
            .push(chip(
                format!("umu-run {}", umu_label(&state.umu)),
                if state.umu.exists() { MUTED } else { WARNING },
            ))
            .push(chip(format!("игр {}", state.games.len()), MUTED))
            .push(chip(
                format!("запущено {running}"),
                if running > 0 { SUCCESS } else { MUTED },
            ))
            .push(chip(format!("раннеров {runner_count}"), MUTED))
            .push(container(Space::new().width(Length::Fill)).width(Length::Fill))
            .push(chip("Ctrl+S сохранить", MUTED))
            .push(chip("Ctrl+R перечитать", MUTED))
            .push(chip("Ctrl+Q выйти и сохранить", MUTED))
            .push(chip("Esc отмена", MUTED)),
    );
    Column::with_capacity(1).push(
        container(root.padding(16))
            .width(Length::Fill)
            .height(Length::Fill),
    )
}

/// Пустые состояния — левый список и правая карточка: объяснение плюс кнопки,
/// а не голая строка.
fn empty_state(state: &State) -> Element<'static, Message> {
    let filtering = !state.filter.trim().is_empty();
    container(
        Column::with_capacity(5)
            .spacing(9)
            .align_x(Alignment::Center)
            .push(text(if filtering { "⌕" } else { "▶" }).size(24).color(LINE))
            .push(text(if filtering {
                "По фильтру ничего не найдено"
            } else {
                "Библиотека пуста"
            }))
            .push(
                text(empty_hint(state, filtering))
                    .size(11)
                    .color(MUTED)
                    .wrapping(text::Wrapping::Word),
            )
            .push(
                Row::with_capacity(2)
                    .spacing(8)
                    .push(action("новая игра", Message::New))
                    .push(ghost("перечитать конфиг", Message::Reload)),
            ),
    )
    .padding([4, 10])
    .width(Length::Fill)
    .into()
}

/// Правая панель без выбора: кнопки уже есть в списке слева, поэтому здесь
/// только объяснение — что появится, когда игру выберут.
fn pane_empty(state: &State) -> Element<'static, Message> {
    let body = if state.games.is_empty() {
        "добавьте игру — здесь появятся её путь, wine-префикс, GAMEID и команда запуска"
    } else {
        "выберите игру в списке слева — здесь появятся её путь, wine-префикс, GAMEID и команда \
         запуска, а также кнопки запуска, редактирования и удаления"
    };
    container(
        Column::with_capacity(1)
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .push(
                text(body)
                    .size(11)
                    .color(MUTED)
                    .align_x(Alignment::Center)
                    .wrapping(text::Wrapping::Word),
            ),
    )
    // Вертикаль внутри scrollable не растянуть (высота не ограничена), поэтому
    // подсказка стоит сразу под заголовком: карточка сохраняет размер, когда
    // игру выберут, и список не прыгает.
    .width(Length::Fill)
    .padding([14, 10])
    .into()
}

/// Панель загрузки GE-Proton: версии, прогресс установки, что уже лежит в
/// каталоге. Список даёт GitHub API, поэтому он появляется только по кнопке.
fn dl_body(state: &State) -> Element<'_, Message> {
    let dir = proton_install_dir();
    let job = state.dl_job.as_ref();
    let busy = job.is_some();
    let listing = state.dl_status == DlStatus::Listing;
    let status = if let Some(progress) = job {
        let written = human_size(progress.written());
        if progress.cancelled() {
            format!("{} — прерываем", progress.stage().label())
        } else {
            match progress.fraction() {
                Some(part) => format!(
                    "{} · {} из {} ({:.0}%)",
                    progress.stage().label(),
                    written,
                    human_size(progress.total().unwrap_or_default()),
                    part * 100.0
                ),
                None => format!("{} · {written}", progress.stage().label()),
            }
        }
    } else if listing {
        "запрос к GitHub…".to_string()
    } else if state.dl_releases.is_empty() {
        "версии ещё не запрошены".to_string()
    } else {
        "список готов".to_string()
    };

    let mut col = Column::with_capacity(10)
        .spacing(8)
        .push(
            text(format!(
                "сборки GE-Proton с GitHub. Установка в {} — эта папка и так входит в поиск \
                 раннеров, поэтому новый GE-Proton сразу виден в списках выбора и уходит в \
                 PROTONPATH при запуске.",
                dir.display()
            ))
            .size(11)
            .color(MUTED)
            .wrapping(text::Wrapping::Word),
        )
        .push(
            Row::with_capacity(4)
                .spacing(8)
                .align_y(Alignment::Center)
                .push(
                    button(text("обновить список").size(12))
                        .padding([7, 13])
                        .style(ghost_style)
                        .on_press_maybe((!busy && !listing).then_some(Message::DlList)),
                )
                .push(chip(status, if busy || listing { PRIMARY } else { MUTED }))
                .push(container(Space::new().width(Length::Fill)))
                .push(ghost("закрыть", Message::DlToggle)),
        );

    if let Some(part) = job.and_then(|p| p.fraction()) {
        col = col.push(progress_bar(0.0..=1.0, part).girth(Length::Fixed(8.0)));
    }

    if state.dl_releases.is_empty() {
        col = col.push(
            text(if listing {
                "ждём ответ GitHub…"
            } else {
                "список пуст: обращения к GitHub лимитированы без токена, если 403 — попробуйте через час"
            })
            .size(11)
            .color(MUTED)
            .wrapping(text::Wrapping::Word),
        );
    } else {
        col = col.push(labeled(
            "версия",
            pick_list(
                state.dl_releases.as_slice(),
                state.dl_chosen.as_ref(),
                Message::DlChosen,
            )
            .text_size(13)
            .padding([7, 10])
            .width(Length::Fill),
        ));
        if let Some(release) = &state.dl_chosen {
            let already = installed_as(&state.dl_installed, &release.tag).is_some();
            let mut actions = Row::with_capacity(2)
                .spacing(8)
                .align_y(Alignment::Center)
                .push(
                    button(
                        text(if already {
                            "уже установлен"
                        } else {
                            "скачать и установить"
                        })
                        .size(12),
                    )
                    .padding([7, 14])
                    .style(primary_style)
                    .on_press_maybe((!busy && !listing && !already).then_some(Message::DlInstall)),
                );
            if busy {
                actions = actions.push(danger("прервать", Message::DlCancel));
            }
            col = col
                .push(kv("размер архива", human_size(release.size)))
                .push(kv("опубликован", release.date.clone()))
                .push(kv(
                    "проверка",
                    match &release.sum {
                        Some((_, algo)) => format!("{}, сверяется до распаковки", algo.label()),
                        None => "релиз не публикует контрольную сумму".to_string(),
                    },
                ))
                .push(actions);
        }
    }

    if let Some(path) = &state.dl_last {
        col = col.push(
            Row::with_capacity(2)
                .spacing(8)
                .align_y(Alignment::Center)
                .push(ghost(
                    "сделать раннером по умолчанию",
                    Message::DefaultRunner(Runner {
                        path: Some(path.display().to_string()),
                    }),
                ))
                .push(
                    text("выбор не записан: «сохранить» или Ctrl+S")
                        .size(10)
                        .color(MUTED),
                ),
        );
    }

    if let Some(e) = &state.dl_error {
        col = col.push(banner("✕", "ошибка", e.clone(), DANGER));
    }

    col = col.push(
        Row::with_capacity(2)
            .spacing(8)
            .align_y(Alignment::Center)
            .push(label("уже в этой папке"))
            .push(chip(
                format!("{}", state.dl_installed.len()),
                if state.dl_installed.is_empty() {
                    MUTED
                } else {
                    SUCCESS
                },
            )),
    );
    col = col.push(
        container(
            text(if state.dl_installed.is_empty() {
                "пока ничего".to_string()
            } else {
                state.dl_installed.join(" · ")
            })
            .size(11)
            .color(TEXT)
            .wrapping(text::Wrapping::WordOrGlyph),
        )
        .padding(10)
        .width(Length::Fill)
        .style(sunken_style),
    );
    col.into()
}

fn game_row<'a>(state: &'a State, i: usize, g: &'a Game) -> Element<'a, Message> {
    let running = g.child.is_some();
    let selected = state.selected == Some(i);
    let (mark, mark_color) = if running {
        ("●", SUCCESS)
    } else if g.exit.is_some() {
        ("●", status_color(g))
    } else {
        ("○", LINE)
    };
    let inner = Row::with_capacity(5)
        .spacing(10)
        .align_y(Alignment::Center)
        .push(
            container(Space::new().width(3.0))
                .height(Length::Fill)
                .style(move |_: &Theme| container::Style {
                    background: Some(Background::Color(if selected {
                        PRIMARY
                    } else {
                        Color::TRANSPARENT
                    })),
                    border: Border {
                        radius: Radius::from(2.0),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
        )
        .push(text(mark).size(9).color(mark_color))
        .push(
            Column::with_capacity(2)
                .spacing(2)
                .width(Length::Fill)
                .push(text(g.cfg.name.clone()).size(14))
                .push(text(sub_line(g)).size(11).color(status_color(g))),
        )
        .push(chip(runner_tag(g, state), RUNNER_TINT))
        .push(if running {
            danger("стоп", Message::Stop(i))
        } else {
            action("▶  запустить", Message::Start(i))
        });
    button(inner)
        .padding([9, 11])
        .width(Length::Fill)
        .style(row_style(selected))
        .on_press(Message::Select(i))
        .into()
}

fn delete_button(state: &State) -> Element<'static, Message> {
    match state.selected {
        Some(i) if state.confirm_delete == Some(i) => danger("удалить точно?", Message::Delete(i)),
        Some(i) => ghost("удалить", Message::Delete(i)),
        None => button(text("удалить").size(12))
            .padding([6, 12])
            .style(ghost_style)
            .into(),
    }
}

fn empty_hint(state: &State, has_games: bool) -> String {
    if has_games {
        format!(
            "из {} игр ни одна не подходит под «{}»",
            state.games.len(),
            state.filter.trim()
        )
    } else {
        format!(
            "«новая игра» добавит игру вручную, а поиск найдёт их в библиотеке Steam. Конфиг: {}",
            config_path().display()
        )
    }
}

fn sub_line(g: &Game) -> String {
    if let Some(child) = &g.child {
        let secs = g.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        format!(
            "идёт {:02}:{:02} · pid {} · {}",
            secs / 60,
            secs % 60,
            child.id(),
            short_path(&g.cfg.exe)
        )
    } else if let Some(e) = &g.exit {
        format!("завершена: {e} · {}", short_path(&g.cfg.exe))
    } else {
        short_path(&g.cfg.exe)
    }
}

fn runner_tag(g: &Game, state: &State) -> String {
    match g.cfg.runner.as_deref().or(state.defaults.runner.as_deref()) {
        Some(p) => Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "авто".into()),
        None => "авто".into(),
    }
}

fn short_path(exe: &str) -> String {
    let parts: Vec<&str> = exe.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() > 3 {
        format!("…/{}/{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        exe.to_string()
    }
}

fn status_color(g: &Game) -> Color {
    if g.child.is_some() {
        SUCCESS
    } else if matches!(&g.exit, Some(e) if e != "вышла нормально") {
        WARNING
    } else {
        MUTED
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_for_filename_and_gameid() {
        assert_eq!(slug("My Game 2"), "my-game-2");
        assert_eq!(slug("  Test Game:  Advanced!!  "), "test-game-advanced");
    }

    #[test]
    fn clip_keeps_char_count() {
        assert_eq!(clip("abc", 5), "abc");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("привет мир", 7).chars().count(), 7);
    }

    #[test]
    fn args_respect_quotes() {
        assert_eq!(
            split_args("-w 1920x1080 --fps \"two words\""),
            ["-w", "1920x1080", "--fps", "two words"]
        );
        assert_eq!(split_args(""), Vec::<String>::new());
    }

    #[test]
    fn env_pairs() {
        let map = parse_env("A=1; B=2\nC=x=y ; мусор");
        assert_eq!(map.get("A").map(String::as_str), Some("1"));
        assert_eq!(map.get("B").map(String::as_str), Some("2"));
        assert_eq!(map.get("C").map(String::as_str), Some("x=y"));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn config_schema_roundtrip() {
        let raw = r#"
umu_run = "~/bin/umu-run"

[defaults]
runner = "~/.local/share/Steam/compatibilitytools.d/proton-upstream"
store = "none"
prefix_root = "~/Games/umu"
runner_dirs = ["~/.local/share/lutris/runners/proton"]

[[game]]
name = "My Game"
exe = "~/Games/MyGame/MyGame.exe"
gameid = "um-my-game"
prefix = "~/Games/umu/um-my-game"
args = ["-windowed"]

[game.env]
DXVK_HUD = "compiler"
"#;
        let cfg: Config = toml::from_str(raw).expect("схема конфига читается");
        assert_eq!(cfg.game.len(), 1);
        assert_eq!(cfg.game[0].args, ["-windowed"]);
        assert_eq!(
            cfg.defaults.runner_dirs,
            ["~/.local/share/lutris/runners/proton"]
        );
        assert_eq!(cfg.game[0].env.len(), 1);
        let back = toml::to_string_pretty(&cfg).expect("сериализуется");
        let again: Config = toml::from_str(&back).expect("читается обратно");
        assert_eq!(again.game[0].name, "My Game");
        assert_eq!(again.game[0].exe, cfg.game[0].exe);
        assert_eq!(again.defaults.runner, cfg.defaults.runner);
        assert_eq!(again.game[0].prefix, cfg.game[0].prefix);
    }

    #[test]
    fn shell_quotes_only_when_needed() {
        assert_eq!(shq("/home/u/game/Play.exe"), "/home/u/game/Play.exe");
        assert_eq!(shq("/games/some game/x.exe"), "'/games/some game/x.exe'");
        assert_eq!(shq("it's"), r#"'it'\''s'"#);
        assert_eq!(shq(""), "''");
    }

    #[test]
    fn kill_tree_reaches_children_too() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30 & sleep 30; wait")
            .spawn()
            .expect("тестовое дерево процессов");
        let pid = child.id();
        let mut kids: Vec<u32> = Vec::new();
        for _ in 0..40 {
            kids = children_of(pid);
            if !kids.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!kids.is_empty(), "потомки не успели родиться");
        assert!(kill_tree(pid) >= 2, "TERM ушёл только одному процессу");
        let _ = child.wait();
        for p in kids {
            unsafe { libc::kill(p as libc::pid_t, libc::SIGKILL) };
        }
    }

    #[test]
    fn plan_is_one_copy_pasteable_shell_line() {
        let dir = std::env::temp_dir().join(format!("umulaunch-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("временный каталог");
        let exe = dir.join("My Game.exe");
        std::fs::write(&exe, b"MZ").expect("мнимый .exe");
        let cfg = GameCfg {
            name: "My Game".into(),
            exe: exe.display().to_string(),
            args: vec!["-w 1920".into()],
            ..Default::default()
        };
        let desc = plan(&cfg, Path::new("umu-run"), &Defaults::default()).expect("план собирается");
        assert_eq!(desc.lines().count(), 1, "не одна строка: {desc}");
        assert!(desc.starts_with("cd ") && desc.contains(" && "), "{desc}");
        assert!(desc.contains("GAMEID=my-game"), "{desc}");
        assert!(
            desc.contains("'-w 1920'"),
            "аргумент с пробелом без кавычек: {desc}"
        );
        assert!(
            desc.contains("/My Game.exe'"),
            "путь с пробелом без кавычек: {desc}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_dirs_cleans_the_input_line() {
        assert_eq!(split_dirs(" ~/a ; ~/b;; ~/a ;"), ["~/a", "~/b", "~/a"]);
        assert_eq!(split_dirs(""), Vec::<String>::new());
        assert_eq!(split_dirs("  "), Vec::<String>::new());
    }

    #[test]
    fn vdf_paths_are_picked_out_of_the_library_list() {
        let vdf = r#"
"libraryfolders"
{
    "0"
    {
        "path"        "/home/user/.local/share/Steam"
        "label"       ""
        "contentid"   "1451836550285288890"
        "apps"
        {
            "244850"  "90721"
        }
    }
    "1"
    {
        "path"        "/home/user/SteamLibrary"
    }
}
"#;
        assert_eq!(
            library_paths_from_vdf(vdf),
            ["/home/user/.local/share/Steam", "/home/user/SteamLibrary"]
        );
        assert_eq!(library_paths_from_vdf("\"path\"\n{"), Vec::<String>::new());
    }

    #[test]
    fn only_proton_dirs_are_looked_at_in_a_game_library() {
        assert!(proton_in_name(Path::new(
            "/lib/steamapps/common/Proton 10.0"
        )));
        assert!(proton_in_name(Path::new(
            "/lib/steamapps/common/Proton - Experimental"
        )));
        assert!(proton_in_name(Path::new("/x/GE-Proton11-11")));
        assert!(!proton_in_name(Path::new("/lib/steamapps/common/game")));
        assert!(!proton_in_name(Path::new(
            "/lib/steamapps/common/SteamLinuxRuntime_sniper"
        )));
    }

    #[test]
    fn candidates_include_umu_cache_and_portproton_roots() {
        let names = runner_search_candidates(&[])
            .into_iter()
            .map(|d| d.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");
        // umu скачивает GE-Proton в $XDG_CACHE_HOME/umu — без этого раннер
        // лежит на диске, но в списке не появляется
        for want in [
            "/.cache/umu",
            "/.local/share/PortProton",
            "/opt/PortProton",
            "ru.linux_gaming.PortProton",
            "com.valvesoftware.Steam",
        ] {
            assert!(names.contains(want), "в поиске нет {want}: {names}");
        }
    }

    #[test]
    fn portal_uri_becomes_a_plain_path() {
        assert_eq!(
            uri_to_path("file:///home/user/Games/game.exe").unwrap(),
            PathBuf::from("/home/user/Games/game.exe")
        );
        // портал присылает URI, поэтому пробел и кириллица приходят в
        // percent-encoding: «/run/media/user/Диск 1/game.exe»
        assert_eq!(
            uri_to_path("file:///run/media/user/%D0%94%D0%B8%D1%81%D0%BA%201/game.exe").unwrap(),
            PathBuf::from("/run/media/user/Диск 1/game.exe")
        );
        assert_eq!(
            uri_to_path("file:///home/user/Games/Proton%2010.0/game.exe").unwrap(),
            PathBuf::from("/home/user/Games/Proton 10.0/game.exe")
        );
        // чужой хост в file:// — это не локальный файл, лаунчер его не откроет
        assert_eq!(uri_to_path("file://other/share/game.exe"), None);
        assert_eq!(uri_to_path("https://example.com/game.exe"), None);
        // битый percent-encoding остаётся ошибкой, а не мусорным путём
        assert_eq!(uri_to_path("file:///tmp/%FF"), None);
        // одинокий % — допустимое имя файла, оно и есть путь
        assert_eq!(
            uri_to_path("file:///tmp/%").unwrap(),
            PathBuf::from("/tmp/%")
        );
    }

    #[test]
    fn only_file_targets_get_filters() {
        let labels = pick_filters(PickTarget::DraftExe)
            .iter()
            .map(|f| f.label().to_string())
            .collect::<Vec<_>>();
        assert_eq!(labels, ["Windows-программы", "все файлы"]);
        assert_eq!(pick_filters(PickTarget::UmuRun).len(), 2);
        // каталоги фильтрует сам портал: там выбор папки, а не файла
        assert!(pick_filters(PickTarget::ScanDir).is_empty());
        assert!(pick_filters(PickTarget::RunnerDirs).is_empty());
        assert!(PickTarget::DraftExe.wants_file());
        assert!(!PickTarget::DraftPrefix.wants_file());
    }

    #[test]
    fn closing_the_dialog_is_not_an_error() {
        assert!(is_declined(&ashpd::Error::Response(
            ResponseError::Cancelled
        )));
        // gtk/kde сообщают «other», когда диалог закрыли без выбора
        assert!(is_declined(&ashpd::Error::Response(ResponseError::Other)));
        assert!(is_declined(&ashpd::Error::Portal(
            PortalError::WindowDestroyed(String::new())
        )));
        // а вот отсутствие портала — настоящая ошибка, её надо показать
        let e = ashpd::Error::IO(std::io::Error::other("нет dbus"));
        assert!(!is_declined(&e));
        assert!(portal_error(e).contains("системный диалог не открылся"));
    }

    #[test]
    fn dialog_starts_at_the_nearest_existing_folder() {
        let dir = std::env::temp_dir().join(format!("umulaunch-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("deep")).expect("каталог");
        let file = dir.join("deep/game.exe");
        std::fs::write(&file, b"MZ").expect("файл");

        assert_eq!(start_dir(&file.display().to_string()), dir.join("deep"));
        assert_eq!(
            start_dir(&dir.join("deep").display().to_string()),
            dir.join("deep")
        );
        // несуществующий хвост поднимаем до ближайшей существующей папки
        assert_eq!(
            start_dir(&dir.join("deep/nope/x").display().to_string()),
            dir.join("deep")
        );
        assert!(start_dir("~/Games/нет/такой/папки").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_request_is_the_only_window_event_that_quits() {
        let id = iced::window::Id::unique();
        let close = iced::Event::Window(iced::window::Event::CloseRequested);
        assert!(
            matches!(
                on_window_event(close, iced::event::Status::Captured, id),
                Some(Message::Quit)
            ),
            "закрытие окна должно вести к Message::Quit"
        );
        // движение и фокус — не повод прощаться с процессом
        let moved = iced::Event::Window(iced::window::Event::Moved(iced::Point::new(0.0, 0.0)));
        assert!(
            on_window_event(moved, iced::event::Status::Ignored, id).is_none(),
            "перемещение окна ничего не сохраняет"
        );
        let focused = iced::Event::Window(iced::window::Event::Focused);
        assert!(
            on_window_event(focused, iced::event::Status::Ignored, id).is_none(),
            "фокус — не закрытие"
        );
    }

    #[test]
    fn empty_config_is_not_an_error() {
        let cfg: Config = toml::from_str("").expect("пустой конфиг");
        assert!(cfg.game.is_empty());
        assert!(cfg.defaults.runner.is_none());
    }
    /// Как GitHub отдаёт релизы GE-Proton: два архива по архитектурам плюс
    /// `.sha512sum`; старые релизы — один архив без контрольной суммы.
    fn fake_releases() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({
                "tag_name": "GE-Proton11-11",
                "draft": false,
                "published_at": "2026-10-19T18:20:53Z",
                "assets": [
                    {"name": "GE-Proton11-11-aarch64.tar.gz", "size": 648071663,
                     "browser_download_url": "https://gh/aarch64.tar.gz"},
                    {"name": "GE-Proton11-11-aarch64.sha512sum", "size": 160,
                     "browser_download_url": "https://gh/aarch64.sha512sum"},
                    {"name": "GE-Proton11-11-x86_64.tar.gz", "size": 564789552,
                     "browser_download_url": "https://gh/x86_64.tar.gz"},
                    {"name": "GE-Proton11-11-x86_64.sha512sum", "size": 159,
                     "browser_download_url": "https://gh/x86_64.sha512sum"},
                ]
            }),
            serde_json::json!({
                "tag_name": "Proton-5.9-GE-6-ST",
                "draft": false,
                "published_at": "2020-11-26T10:00:00Z",
                "assets": [
                    {"name": "Proton-5.9-GE-6-ST.tar.gz", "size": 219000000,
                     "browser_download_url": "https://gh/old.tar.gz"}
                ]
            }),
            serde_json::json!({
                "tag_name": "../evil",
                "draft": false,
                "assets": [
                    {"name": "../evil.tar.gz", "size": 10,
                     "browser_download_url": "https://gh/evil.tar.gz"}
                ]
            }),
        ]
    }

    #[test]
    fn release_listing_prefers_host_arch_and_sum() {
        let x86 = releases_for_arch(&fake_releases(), "x86_64", &["aarch64", "arm64"]);
        assert_eq!(x86.len(), 2, "негодный тег пропускается");
        assert_eq!(x86[0].tag, "GE-Proton11-11");
        assert_eq!(x86[0].url, "https://gh/x86_64.tar.gz");
        assert_eq!(x86[0].size, 564789552);
        assert_eq!(x86[0].date, "2026-10-19");
        assert_eq!(x86[0].sum.as_ref().map(|(_, a)| *a), Some(Algo::Sha512));
        // старый релиз без контрольной суммы остаётся, но без проверки
        assert_eq!(x86[1].tag, "Proton-5.9-GE-6-ST");
        assert!(x86[1].sum.is_none());

        let arm = releases_for_arch(&fake_releases(), "aarch64", &["x86_64", "amd64"]);
        assert_eq!(
            arm.len(),
            1,
            "архив без отметки архитектуры на ARM не предлагаем"
        );
        assert_eq!(arm[0].url, "https://gh/aarch64.tar.gz");
        assert_eq!(arm[0].sum.as_ref().map(|(_, a)| *a), Some(Algo::Sha512));
    }

    #[test]
    fn checksum_file_parses_and_rejects_garbage() {
        let hex = "a".repeat(128);
        let body = format!("{hex}  GE-Proton11-11-x86_64.tar.gz\n");
        assert_eq!(parse_sum(&body, Algo::Sha512).unwrap(), hex);
        assert_eq!(
            parse_sum(&format!("{}  f", hex.to_uppercase()), Algo::Sha512).unwrap(),
            hex
        );
        assert!(parse_sum("not a hash  file.tar.gz", Algo::Sha512).is_err());
        assert!(parse_sum(&"a".repeat(64), Algo::Sha512).is_err());
        assert!(parse_sum(&"a".repeat(64), Algo::Sha256).is_ok());
        assert_eq!(
            algo_of("GE-Proton11-11-x86_64.sha512sum"),
            Some(Algo::Sha512)
        );
        assert_eq!(algo_of("UMU-Proton9-1.sum"), Some(Algo::Sha256));
        assert_eq!(algo_of("GE-Proton11-11.tar.gz"), None);
    }

    #[test]
    fn installed_runner_is_found_by_tag_with_a_boundary() {
        let names = [
            "GE-Proton11-11-x86_64".to_string(),
            "Proton-GE-9-0".to_string(),
            "GE-Proton10-34".to_string(),
        ];
        // архив раскладывается в каталог с суффиксом архитектуры
        assert_eq!(
            installed_as(&names, "GE-Proton11-11"),
            Some("GE-Proton11-11-x86_64")
        );
        assert_eq!(
            installed_as(&names, "GE-Proton10-34"),
            Some("GE-Proton10-34")
        );
        assert_eq!(installed_as(&names, "GE-9-0"), Some("Proton-GE-9-0"));
        // префикс без границы — не совпадение
        assert_eq!(installed_as(&names, "GE-Proton11-1"), None);
        assert_eq!(installed_as(&names, "GE-Proton10-3"), None);
        assert_eq!(installed_as(&names, "GE-Proton11-12"), None);
    }

    #[test]
    fn tags_become_catalog_names_safely() {
        assert!(safe_tag("GE-Proton11-11"));
        assert!(safe_tag("Proton-5.9-GE-6-ST"));
        for bad in [
            "",
            "..",
            "../evil",
            "GE Proton",
            "a/b",
            "a\\b",
            "x;y",
            "a..b",
        ] {
            assert!(!safe_tag(bad), "{bad} не должен проходить");
        }
    }

    #[test]
    fn archive_paths_cannot_escape_install_dir() {
        assert!(!escapes_dir(Path::new("GE-Proton11-11/proton")));
        assert!(escapes_dir(Path::new("/etc/passwd")));
        assert!(escapes_dir(Path::new("GE-Proton11-11/../../etc/passwd")));
        // относительная ссылка внутри раннера допустима
        assert!(!escapes_link(
            Path::new("GE-Proton11-11/files/bin/x"),
            Path::new("../../lib/liby.so")
        ));
        // …а ведущая выше папки распаковки — нет, даже если потом «возвращается»
        assert!(escapes_link(
            Path::new("GE-Proton11-11/files/bin/x"),
            Path::new("../../../../etc/passwd")
        ));
        assert!(escapes_link(
            Path::new("a/b/x"),
            Path::new("../../../../etc/passwd")
        ));
        assert!(escapes_link(
            Path::new("GE-Proton11-11/x"),
            Path::new("/bin/sh")
        ));
    }

    #[test]
    fn sizes_read_like_a_person() {
        assert_eq!(human_size(512), "512 Б");
        assert_eq!(human_size(564_789_552), "539 MiB");
        assert_eq!(human_size(2_190_000_000), "2.0 GiB");
    }

    #[test]
    fn download_progress_reports_stage_and_share() {
        let prog = DlProgress::default();
        assert_eq!(prog.stage(), Stage::Download);
        assert_eq!(prog.fraction(), None, "без размера полосы нет");
        prog.set_total(1000);
        prog.add(250);
        assert_eq!(prog.fraction(), Some(0.25));
        prog.set_stage(Stage::Unpack);
        assert_eq!(prog.stage(), Stage::Unpack);
        assert!(!prog.cancelled());
        prog.cancel();
        assert!(prog.cancelled());
    }

    /// Мини-архив в формате GE-Proton: один верхний каталог и файл `proton`.
    /// `escape` добавляет запись, ведущую за пределы распаковки.
    fn fake_archive(path: &Path, top: &str, escape: bool) {
        use tar::EntryType;
        let file = std::fs::File::create(path).expect("архив создаётся");
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::new(0));
        let mut ar = tar::Builder::new(enc);
        let mut data = b"#!/bin/sh\necho proton".as_slice();
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        ar.append_data(&mut header, format!("{top}/proton"), &mut data)
            .expect("proton в архиве");
        if escape {
            // запись с абсолютным путём: Builder::append_data такие не принимает,
            // поэтому путь кладём в заголовок напрямую
            let mut bad = b"oops".as_slice();
            let mut h2 = tar::Header::new_gnu();
            h2.set_size(bad.len() as u64);
            h2.set_mode(0o644);
            h2.set_entry_type(EntryType::Regular);
            h2.set_path_absolute("/umulaunch-escape")
                .expect("путь в заголовке");
            h2.set_cksum();
            ar.append(&h2, &mut bad).expect("запись добавлена");
        }
        let enc = ar.into_inner().expect("tar закрыт");
        enc.finish().expect("gzip закрыт");
    }

    #[test]
    fn unpack_installs_runner_and_blocks_escapes() {
        let dir = std::env::temp_dir().join(format!("umulaunch-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("временный каталог");
        let prog = DlProgress::default();

        let good = dir.join("good.tar.gz");
        fake_archive(&good, "GE-Proton0-0", false);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("каталог распаковки");
        unpack(&good, &out, &prog).expect("распаковка проходит");
        assert!(out.join("GE-Proton0-0/proton").is_file(), "proton на месте");
        assert!(matches!(layout(&out), Layout::Single(name) if name == "GE-Proton0-0"));

        let evil = dir.join("evil.tar.gz");
        fake_archive(&evil, "GE-Proton0-1", true);
        let err = unpack(&evil, &out, &prog).expect_err("путь наружу нельзя принять");
        assert!(err.contains("вне папки установки"), "{err}");
        assert!(
            !out.join("umulaunch-escape").exists(),
            "запись всё же раскрылась"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_download_stops_before_write() {
        let prog = DlProgress::default();
        prog.cancel();
        let dir = std::env::temp_dir().join(format!("umulaunch-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("временный каталог");
        // локальный сервер тут не нужен: обрыв проверяется на распаковке
        let archive = dir.join("a.tar.gz");
        fake_archive(&archive, "GE-Proton0-2", false);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("каталог распаковки");
        assert_eq!(
            unpack(&archive, &out, &prog).unwrap_err(),
            "установка прервана"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
