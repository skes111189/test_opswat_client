#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod activity;
mod journal;
mod model;
#[cfg(any(target_os = "macos", test))]
mod startup;
use chrono::Local;
use core_client::{ClientConfig, Core, Receiver, Upload};
use iced::{
    widget::{
        button, checkbox, column, container, mouse_area, pick_list, progress_bar, row, scrollable,
        stack, text, text_input, Space,
    },
    window, Color, Element, Length, Subscription, Task, Theme,
};
use model::{
    apply_failure, batch_name, event, preserve, statistics, Item, Secret, SettingsSnapshot,
};
use protocol::{
    Activity, BatchId, ContentKind, Destination, Failure, Health, HealthColor, JobId, JobState,
    JobStatus, Level, RebindJob, RegisterJob,
};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

const TITLE: &str = "MetaDefender 安全傳檔工具";
const NAVY: Color = Color::from_rgb(0.055, 0.12, 0.24);
const BLUE: Color = Color::from_rgb(0.12, 0.35, 0.78);
const GREY: Color = Color::from_rgb(0.48, 0.52, 0.58);
const TEAL: Color = Color::from_rgb(0.0, 0.48, 0.53);
#[derive(Clone)]
struct Runtime {
    config: ClientConfig,
    core: Core,
    receiver: Receiver,
}
impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Runtime [credentials redacted]")
    }
}
impl Runtime {
    fn load(path: &std::path::Path) -> Result<Arc<Self>, Failure> {
        let config = ClientConfig::load(path)?;
        Self::from_config(config)
    }
    fn from_config(config: ClientConfig) -> Result<Arc<Self>, Failure> {
        Ok(Arc::new(Self {
            core: Core::new(&config.core)?,
            receiver: Receiver::new(&config.receiver)?,
            config,
        }))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Transfer,
    Activity,
    Settings,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Info,
    Warning,
    Error,
}
impl std::fmt::Display for Filter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::All => "全部層級",
            Self::Info => "資訊",
            Self::Warning => "警告",
            Self::Error => "錯誤",
        })
    }
}
#[derive(Debug, Clone)]
enum Message {
    Ignored,
    Tab(Tab),
    Reload,
    Configured(Result<Arc<Runtime>, Failure>),
    HealthTick,
    PollTick,
    CoreChecked(u64, Result<(), Failure>),
    RulesLoaded(u64, Result<Vec<String>, Failure>),
    ReceiverChecked(u64, Result<Vec<Destination>, Failure>),
    AddFiles,
    AddFolder,
    Dropped(PathBuf),
    Paths(Vec<PathBuf>),
    Expanded(Vec<(PathBuf, u64)>, Option<String>, usize),
    Selected(JobId, bool),
    Remove,
    Clear,
    ConfirmClear,
    Dismiss,
    Rule(String),
    Destination(Destination),
    BatchName(String),
    BatchPassword(Secret),
    Start,
    BatchCreated(BatchId, Result<String, Failure>),
    Uploaded(JobId, u64, Result<RegisterJob, Failure>),
    Status(JobId, u64, Result<JobStatus, Failure>),
    BatchClosed(Result<(), Failure>),
    Context(JobId),
    Retry(JobId),
    Password(JobId),
    FilePassword(Secret),
    SubmitPassword,
    PasswordUploaded(JobId, u64, Result<RebindJob, Failure>),
    TimeFilter(String),
    FileFilter(String),
    EventFilter(String),
    LevelFilter(Filter),
    Export,
    Exported(Result<(), Failure>),
    Drag,
    Minimize,
    Close,
    ConfirmClose,
}
struct App {
    config_path: PathBuf,
    runtime: Option<Arc<Runtime>>,
    generation: u64,
    loading: bool,
    config_error: Option<Failure>,
    core_health: Health,
    receiver_health: Health,
    rules: Vec<String>,
    rules_error: Option<Failure>,
    destinations: Vec<Destination>,
    rule: Option<String>,
    destination: Option<Destination>,
    items: Vec<Item>,
    runtimes: HashMap<JobId, Arc<Runtime>>,
    saved_configs: HashMap<JobId, ClientConfig>,
    tab: Tab,
    name: String,
    password: Secret,
    active_password: Secret,
    first_folder: Option<String>,
    batch: Option<BatchId>,
    active: bool,
    core_batch: Option<(BatchId, String)>,
    close_started: bool,
    upload_slots: Arc<tokio::sync::Semaphore>,
    adding: bool,
    events: Vec<Activity>,
    filter_time: String,
    filter_file: String,
    filter_event: String,
    filter_level: Filter,
    confirm_clear: bool,
    confirm_close: bool,
    password_target: Option<JobId>,
    file_password: Secret,
    context: Option<JobId>,
    restored: bool,
    journal_error: bool,
}
impl App {
    fn new() -> (Self, Task<Message>) {
        let config_path = std::env::args_os()
            .nth(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| "config/client.toml".into());
        let app = Self {
            config_path,
            runtime: None,
            generation: 0,
            loading: false,
            config_error: None,
            core_health: Health::default(),
            receiver_health: Health::default(),
            rules: vec![],
            rules_error: None,
            destinations: vec![],
            rule: None,
            destination: None,
            items: vec![],
            runtimes: HashMap::new(),
            saved_configs: HashMap::new(),
            tab: Tab::Transfer,
            name: String::new(),
            password: Secret::default(),
            active_password: Secret::default(),
            first_folder: None,
            batch: None,
            active: false,
            core_batch: None,
            close_started: false,
            upload_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            adding: false,
            events: vec![],
            filter_time: String::new(),
            filter_file: String::new(),
            filter_event: String::new(),
            filter_level: Filter::All,
            confirm_clear: false,
            confirm_close: false,
            password_target: None,
            file_password: Secret::default(),
            context: None,
            restored: false,
            journal_error: false,
        };
        (app, Task::done(Message::Reload))
    }
    fn log(&mut self, item: Option<JobId>, message: String, level: Level) {
        self.events.push(event(
            item.and_then(|id| self.items.iter().find(|i| i.id == id)),
            message,
            level,
        ));
    }
    fn services_ready(&self) -> bool {
        self.config_error.is_none()
            && !self.loading
            && self.core_health.usable()
            && self.receiver_health.usable()
    }
    fn can_start(&self) -> bool {
        !self.active
            && !self.adding
            && !self.journal_error
            && self.rules_error.is_none()
            && self.services_ready()
            && self.rule.is_some()
            && self.destination.is_some()
            && self.items.iter().any(|i| i.state == JobState::Pending)
    }
    fn check_health(&mut self) -> Task<Message> {
        let Some(runtime) = self.runtime.clone() else {
            return Task::none();
        };
        let mut tasks = vec![];
        let g = self.generation;
        if !self.core_health.in_flight {
            self.core_health.in_flight = true;
            let core = runtime.core.clone();
            tasks.push(Task::perform(async move { core.ready().await }, move |r| {
                Message::CoreChecked(g, r)
            }));
            let core = runtime.core.clone();
            tasks.push(Task::perform(async move { core.rules().await }, move |r| {
                Message::RulesLoaded(g, r)
            }));
        }
        if !self.receiver_health.in_flight {
            self.receiver_health.in_flight = true;
            let receiver = runtime.receiver.clone();
            tasks.push(Task::perform(
                async move {
                    tokio::time::timeout(Duration::from_secs(5), receiver.destinations())
                        .await
                        .unwrap_or_else(|_| {
                            Err(Failure::new("receiver 健康檢查", "檢查逾時", None))
                        })
                },
                move |r| Message::ReceiverChecked(g, r),
            ));
        }
        Task::batch(tasks)
    }
    fn upload_task(
        &mut self,
        id: JobId,
        core_batch: Option<String>,
        password: Secret,
    ) -> Task<Message> {
        let Some(rt) = self.ensure_runtime(id) else {
            return Task::none();
        };
        let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
            return Task::none();
        };
        let Some(settings) = item.settings.clone() else {
            return Task::none();
        };
        item.attempt += 1;
        let attempt = item.attempt;
        item.busy = true;
        item.state = JobState::Uploading;
        item.failure = None;
        let path = item.path.clone();
        let filename = item.filename.clone();
        let slots = self.upload_slots.clone();
        Task::perform(
            async move {
                let _permit = slots
                    .acquire_owned()
                    .await
                    .map_err(|_| Failure::new("上傳", "上傳佇列已關閉", None).safe())?;
                let (sha256, _) = core_client::sha256_file(&path).await?;
                let data_id = rt
                    .core
                    .upload(Upload {
                        job_id: Some(id),
                        path: &path,
                        filename: &filename,
                        rule: &settings.rule,
                        batch: core_batch.as_deref(),
                        password: &password.0,
                        chunk_bytes: rt.config.chunk_bytes,
                        multipart_threshold: rt.config.multipart_threshold,
                    })
                    .await?;
                Ok(RegisterJob {
                    id,
                    batch_id: settings.batch_id,
                    data_id,
                    destination: settings.destination.key,
                    filename,
                    sha256,
                })
            },
            move |r| Message::Uploaded(id, attempt, r),
        )
    }
    fn status_task(&mut self, id: JobId, retry: bool) -> Task<Message> {
        let Some(rt) = self.ensure_runtime(id) else {
            return Task::none();
        };
        let Some(item) = self.items.iter_mut().find(|i| i.id == id) else {
            return Task::none();
        };
        item.busy = true;
        let attempt = item.attempt;
        let registration = item.registration.clone();
        let rebind = item.rebind.clone();
        Task::perform(
            async move {
                let current = rt.receiver.status(id).await;
                let mut status = match current {
                    Ok(s) => s,
                    Err(e) if e.http_status == Some(404) => {
                        if let Some(r) = registration {
                            rt.receiver.register(&r).await?
                        } else {
                            return Err(Failure::new(
                                "恢復工作",
                                "Core 上傳結果未知；需管理者查詢，禁止建立重複掃描",
                                None,
                            ));
                        }
                    }
                    Err(e) => return Err(e),
                };
                if let Some(r) = rebind {
                    status = rt.receiver.rebind(id, &r).await?;
                }
                if retry && status.state == JobState::Failed {
                    status = rt.receiver.retry(id).await?;
                }
                Ok(status)
            },
            move |r| Message::Status(id, attempt, r),
        )
    }
    fn finish_and_close(&mut self) -> Task<Message> {
        let mut tasks = vec![];
        if !self.close_started {
            if let Some((batch, core_batch)) = &self.core_batch {
                if self
                    .items
                    .iter()
                    .filter(|i| i.settings.as_ref().is_some_and(|s| s.batch_id == *batch))
                    .all(|i| i.state != JobState::Uploading)
                {
                    if let Some(rt) = self
                        .items
                        .iter()
                        .find(|i| i.settings.as_ref().is_some_and(|s| s.batch_id == *batch))
                        .and_then(|i| self.runtimes.get(&i.id))
                        .cloned()
                    {
                        self.close_started = true;
                        let id = core_batch.clone();
                        tasks.push(Task::perform(
                            async move { rt.core.close_batch_when_complete(&id).await },
                            Message::BatchClosed,
                        ));
                    }
                }
            }
        }
        let s = statistics(&self.items, self.batch);
        let working = self
            .items
            .iter()
            .any(|i| i.settings.is_some() && (i.busy || !i.state.terminal()));
        if self.active && !working {
            self.name.clear();
            self.password.clear();
            self.active_password.clear();
            self.file_password.clear();
            self.password_target = None;
            self.log(
                None,
                format!(
                    "批次結束：送達 {}（原始檔 {}）、拒絕 {}、失敗 {}",
                    s.delivered, s.original, s.rejected, s.failed
                ),
                Level::Info,
            );
        }
        self.active = working;
        Task::batch(tasks)
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        let save_state = matches!(
            &message,
            Message::BatchCreated(..)
                | Message::Uploaded(..)
                | Message::Status(..)
                | Message::Retry(..)
                | Message::SubmitPassword
                | Message::PasswordUploaded(..)
                | Message::Remove
                | Message::ConfirmClear
                | Message::Clear
                | Message::BatchClosed(..)
        );
        let mut task = Task::none();
        match message {
            Message::Tab(tab) => self.tab = tab,
            Message::Reload if !self.loading => {
                self.loading = true;
                let path = self.config_path.clone();
                return Task::perform(async move { Runtime::load(&path) }, Message::Configured);
            }
            Message::Configured(result) => {
                self.loading = false;
                match result {
                    Ok(rt) => {
                        if self
                            .runtime
                            .as_ref()
                            .is_none_or(|old| old.config.core != rt.config.core)
                        {
                            self.core_health = Health::default();
                        }
                        if self
                            .runtime
                            .as_ref()
                            .is_none_or(|old| old.config.receiver != rt.config.receiver)
                        {
                            self.receiver_health = Health::default();
                        }
                        self.runtime = Some(rt);
                        self.config_error = None;
                        self.generation += 1;
                        if !self.restored {
                            self.restore();
                        }
                        self.core_health.in_flight = false;
                        self.receiver_health.in_flight = false;
                        return self.check_health();
                    }
                    Err(e) => {
                        self.config_error = Some(e.clone());
                        self.log(None, e.to_string(), Level::Error);
                    }
                }
            }
            Message::HealthTick => return self.check_health(),
            Message::CoreChecked(g, result) if g == self.generation => {
                if let Some(e) = self.core_health.finish(result.is_ok()) {
                    self.log(
                        None,
                        format!(
                            "Core：{e}{}",
                            result.err().map(|e| format!("；{e}")).unwrap_or_default()
                        ),
                        if self.core_health.usable() {
                            Level::Info
                        } else {
                            Level::Error
                        },
                    );
                }
            }
            Message::RulesLoaded(g, result) if g == self.generation => match result {
                Ok(rules) => {
                    self.rules_error = None;
                    if preserve(&mut self.rule, &rules, |s| s.as_str()) {
                        self.log(None, "原規則已移除，請重新選擇".into(), Level::Warning);
                    }
                    self.rules = rules;
                }
                Err(e) => {
                    // Ready status reflects /readyz only. Configuration errors disable starting.
                    if self.rules_error.is_none() {
                        self.log(None, e.to_string(), Level::Error);
                    }
                    self.rules_error = Some(e);
                }
            },
            Message::ReceiverChecked(g, result) if g == self.generation => {
                if let Some(e) = self.receiver_health.finish(result.is_ok()) {
                    self.log(
                        None,
                        format!(
                            "receiver：{e}{}",
                            result
                                .as_ref()
                                .err()
                                .map(|e| format!("；{e}"))
                                .unwrap_or_default()
                        ),
                        if self.receiver_health.usable() {
                            Level::Info
                        } else {
                            Level::Error
                        },
                    );
                }
                if let Ok(options) = result {
                    if preserve(&mut self.destination, &options, |d| d.key.as_str()) {
                        self.log(None, "原目的地已移除，請重新選擇".into(), Level::Warning);
                    }
                    self.destinations = options;
                }
            }
            Message::AddFiles if !self.active && !self.adding => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("新增檔案")
                            .pick_files()
                            .await
                            .unwrap_or_default()
                            .into_iter()
                            .map(|f| f.path().to_path_buf())
                            .collect()
                    },
                    Message::Paths,
                )
            }
            Message::AddFolder if !self.active && !self.adding => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("新增資料夾")
                            .pick_folder()
                            .await
                            .map(|f| vec![f.path().to_path_buf()])
                            .unwrap_or_default()
                    },
                    Message::Paths,
                )
            }
            Message::Dropped(path)
                if !self.active
                    && !self.adding
                    && !self.confirm_clear
                    && !self.confirm_close
                    && self.password_target.is_none() =>
            {
                return Task::done(Message::Paths(vec![path]))
            }
            Message::Paths(paths) if !self.active && !self.adding => {
                self.adding = true;
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let mut files = vec![];
                            let mut folder = None;
                            let mut errors = 0;
                            for path in paths {
                                if path.is_dir() {
                                    if folder.is_none() {
                                        folder = path
                                            .file_name()
                                            .map(|s| s.to_string_lossy().into_owned());
                                    }
                                    for e in walkdir::WalkDir::new(path)
                                        .follow_links(false)
                                        .sort_by_file_name()
                                    {
                                        match e {
                                            Ok(e) if e.file_type().is_file() => {
                                                match e.metadata() {
                                                    Ok(m) => match e.path().canonicalize() {
                                                        Ok(p) => files.push((p, m.len())),
                                                        Err(_) => errors += 1,
                                                    },
                                                    Err(_) => errors += 1,
                                                }
                                            }
                                            Err(_) => errors += 1,
                                            _ => (),
                                        }
                                    }
                                } else {
                                    match (path.canonicalize(), path.metadata()) {
                                        (Ok(p), Ok(m)) if m.is_file() => files.push((p, m.len())),
                                        _ => errors += 1,
                                    }
                                }
                            }
                            (files, folder, errors)
                        })
                        .await
                        .unwrap_or_default()
                    },
                    |(files, folder, errors)| Message::Expanded(files, folder, errors),
                );
            }
            Message::Expanded(files, folder, mut errors) => {
                self.adding = false;
                if !self.active {
                    if self.first_folder.is_none() {
                        self.first_folder = folder;
                    }
                    let mut seen = model::unique_paths(&self.items);
                    let mut count = 0;
                    for (path, bytes) in files {
                        if seen.insert(path.clone()) {
                            if let Some(i) = Item::new(path, bytes) {
                                if !protocol::valid_filename(&i.filename) || i.filename.len() > 200
                                {
                                    errors += 1;
                                    continue;
                                }
                                self.items.push(i);
                                count += 1;
                            } else {
                                errors += 1;
                            }
                        }
                    }
                    self.log(
                        None,
                        format!("新增 {count} 個檔案，略過 {errors} 個無法讀取或不支援的檔名"),
                        if errors > 0 {
                            Level::Warning
                        } else {
                            Level::Info
                        },
                    );
                }
            }
            Message::Selected(id, selected) if !self.active => {
                if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
                    i.selected = selected;
                }
            }
            Message::Remove if !self.active => {
                self.items.retain(|i| !i.selected);
                self.prune();
            }
            Message::Clear if !self.active => {
                if self.items.iter().any(|i| i.state == JobState::Pending) {
                    self.confirm_clear = true;
                } else {
                    self.items.clear();
                    self.prune();
                }
            }
            Message::ConfirmClear if !self.active => {
                self.items.clear();
                self.prune();
                self.confirm_clear = false;
            }
            Message::Dismiss => {
                self.confirm_clear = false;
                self.confirm_close = false;
                self.password_target = None;
                self.file_password.clear();
                self.context = None;
            }
            Message::Rule(r) if !self.active => self.rule = Some(r),
            Message::Destination(d) if !self.active => self.destination = Some(d),
            Message::BatchName(name) if !self.active => self.name = name,
            Message::BatchPassword(p) if !self.active => self.password = p,
            Message::Start if self.can_start() => {
                let rt = self.runtime.clone().expect("validated runtime");
                let id = Uuid::new_v4();
                let settings = SettingsSnapshot {
                    batch_id: id,
                    name: batch_name(&self.name, self.first_folder.as_deref()),
                    rule: self.rule.clone().expect("validated rule"),
                    destination: self.destination.clone().expect("validated destination"),
                };
                self.batch = Some(id);
                self.active = true;
                self.active_password = self.password.clone();
                self.core_batch = None;
                self.close_started = false;
                for i in &mut self.items {
                    if i.state == JobState::Pending {
                        i.settings = Some(settings.clone());
                        i.state = JobState::Uploading;
                        i.busy = true;
                        self.runtimes.insert(i.id, rt.clone());
                        self.saved_configs.insert(i.id, rt.config.clone());
                    }
                }
                self.log(None, format!("開始批次：{}", settings.name), Level::Info);
                self.persist();
                if self.journal_error {
                    for i in self
                        .items
                        .iter_mut()
                        .filter(|i| i.settings.as_ref().is_some_and(|s| s.batch_id == id))
                    {
                        apply_failure(
                            i,
                            Failure::new("開始批次", "本機工作紀錄未保存；尚未上傳檔案", None)
                                .safe(),
                        );
                    }
                    return self.finish_and_close();
                }
                return Task::perform(
                    async move { rt.core.create_batch(&settings.rule, &settings.name).await },
                    move |r| Message::BatchCreated(id, r),
                );
            }
            Message::BatchCreated(batch, result) if self.batch == Some(batch) => {
                let ids: Vec<_> = self
                    .items
                    .iter()
                    .filter(|i| i.settings.as_ref().is_some_and(|s| s.batch_id == batch))
                    .map(|i| i.id)
                    .collect();
                match result {
                    Ok(core_batch) => {
                        self.core_batch = Some((batch, core_batch.clone()));
                        task = Task::batch(
                            ids.into_iter()
                                .map(|id| {
                                    self.upload_task(
                                        id,
                                        Some(core_batch.clone()),
                                        self.active_password.clone(),
                                    )
                                })
                                .collect::<Vec<_>>(),
                        );
                    }
                    Err(mut e) => {
                        // No file submission has occurred, regardless of whether an empty Core batch exists.
                        e.retry_safe = true;
                        for id in ids {
                            if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
                                apply_failure(i, e.clone());
                            }
                        }
                        self.log(None, e.to_string(), Level::Error);
                    }
                }
            }
            Message::Uploaded(id, attempt, result) if self.current(id, attempt) => match result {
                Ok(registration) => {
                    let i = self.items.iter_mut().find(|i| i.id == id).unwrap();
                    i.registration = Some(registration.clone());
                    i.state = JobState::Scanning;
                    let rt = self.runtimes[&id].clone();
                    self.log(Some(id), "上傳完成，登記 receiver 工作".into(), Level::Info);
                    task = Task::perform(
                        async move { rt.receiver.register(&registration).await },
                        move |r| Message::Status(id, attempt, r),
                    );
                }
                Err(e) => self.fail_item(id, e),
            },
            Message::Status(id, attempt, result) if self.current(id, attempt) => match result {
                Ok(status) if status.registration.id == id => {
                    let i = self.items.iter_mut().find(|i| i.id == id).unwrap();
                    let changed = i.state != status.state || i.content != status.content;
                    i.busy = false;
                    i.state = status.state;
                    i.content = status.content;
                    i.failure = status.failure.clone();
                    i.registration = Some(status.registration);
                    i.rebind = None;
                    if changed {
                        let label = i.label().to_string();
                        let detail = i
                            .failure
                            .as_ref()
                            .map(|e| format!("；{e}"))
                            .unwrap_or_default();
                        self.log(
                            Some(id),
                            format!("{label}{detail}"),
                            match status.state {
                                JobState::Failed | JobState::Rejected => Level::Error,
                                JobState::NeedsPassword => Level::Warning,
                                _ => Level::Info,
                            },
                        );
                    }
                    if status.state == JobState::AwaitingOriginal {
                        let i = self.items.iter_mut().find(|i| i.id == id).unwrap();
                        i.busy = true;
                        let path = i.path.clone();
                        let rt = self.runtimes[&id].clone();
                        task = Task::perform(
                            async move { rt.receiver.original(id, &path).await },
                            move |r| Message::Status(id, attempt, r),
                        );
                    }
                }
                Ok(_) => self.fail_item(id, Failure::new("查詢工作", "工作 ID 不符", None)),
                Err(e) => self.fail_item(id, e),
            },
            Message::PollTick => {
                let ids: Vec<_> = self
                    .items
                    .iter()
                    .filter(|i| {
                        !i.busy
                            && matches!(
                                i.state,
                                JobState::Scanning
                                    | JobState::Delivering
                                    | JobState::AwaitingOriginal
                            )
                    })
                    .map(|i| i.id)
                    .collect();
                task = Task::batch(
                    ids.into_iter()
                        .map(|id| self.status_task(id, false))
                        .collect::<Vec<_>>(),
                );
            }
            Message::Context(id) => {
                self.context = if self
                    .items
                    .iter()
                    .any(|i| i.id == id && i.retry_enabled(self.services_ready()))
                {
                    Some(id)
                } else {
                    None
                };
            }
            Message::Retry(id)
                if self
                    .items
                    .iter()
                    .any(|i| i.id == id && i.retry_enabled(self.services_ready())) =>
            {
                self.context = None;
                let i = self.items.iter().find(|i| i.id == id).unwrap();
                let new_upload =
                    i.registration.is_none() && i.failure.as_ref().is_some_and(|e| e.retry_safe);
                if !self.active {
                    self.batch = i.settings.as_ref().map(|s| s.batch_id);
                }
                self.log(Some(id), "手動重試：核對並恢復原工作".into(), Level::Info);
                task = if new_upload {
                    self.upload_task(id, None, Secret::default())
                } else {
                    self.status_task(id, true)
                };
            }
            Message::Password(id)
                if self.services_ready()
                    && self
                        .items
                        .iter()
                        .any(|i| i.id == id && i.state == JobState::NeedsPassword && !i.busy) =>
            {
                self.password_target = Some(id);
                self.file_password.clear();
            }
            Message::FilePassword(p) => self.file_password = p,
            Message::SubmitPassword if !self.file_password.0.is_empty() => {
                if let Some(id) = self.password_target.take() {
                    if !self.services_ready() {
                        return Task::none();
                    }
                    let Some(i) = self
                        .items
                        .iter_mut()
                        .find(|i| i.id == id && i.state == JobState::NeedsPassword)
                    else {
                        return Task::none();
                    };
                    let Some(settings) = i.settings.clone() else {
                        return Task::none();
                    };
                    let Some(registration) = i.registration.clone() else {
                        return Task::none();
                    };
                    i.attempt += 1;
                    let attempt = i.attempt;
                    i.busy = true;
                    i.state = JobState::Uploading;
                    let path = i.path.clone();
                    let filename = i.filename.clone();
                    let rt = self.runtimes[&id].clone();
                    let password = self.file_password.clone();
                    self.file_password.clear();
                    task = Task::perform(
                        async move {
                            let (sha, _) = core_client::sha256_file(&path).await?;
                            if sha != registration.sha256 {
                                return Err(Failure::new(
                                    "密碼重送",
                                    "原始檔已變更，不能更新原工作",
                                    None,
                                ));
                            }
                            let data_id = rt
                                .core
                                .upload(Upload {
                                    job_id: Some(id),
                                    path: &path,
                                    filename: &filename,
                                    rule: &settings.rule,
                                    batch: None,
                                    password: &password.0,
                                    chunk_bytes: rt.config.chunk_bytes,
                                    multipart_threshold: rt.config.multipart_threshold,
                                })
                                .await?;
                            Ok(RebindJob {
                                previous_data_id: registration.data_id,
                                data_id,
                            })
                        },
                        move |r| Message::PasswordUploaded(id, attempt, r),
                    );
                }
            }
            Message::PasswordUploaded(id, attempt, result) if self.current(id, attempt) => {
                match result {
                    Ok(rebind) => {
                        let i = self.items.iter_mut().find(|i| i.id == id).unwrap();
                        i.rebind = Some(rebind.clone());
                        let rt = self.runtimes[&id].clone();
                        task = Task::perform(
                            async move { rt.receiver.rebind(id, &rebind).await },
                            move |r| Message::Status(id, attempt, r),
                        );
                    }
                    Err(e) => self.fail_item(id, e),
                }
            }
            Message::BatchClosed(result) => match result {
                Ok(()) => self.log(None, "Core 批次已關閉".into(), Level::Info),
                Err(e) => self.log(None, e.to_string(), Level::Warning),
            },
            Message::TimeFilter(v) => self.filter_time = v,
            Message::FileFilter(v) => self.filter_file = v,
            Message::EventFilter(v) => self.filter_event = v,
            Message::LevelFilter(v) => self.filter_level = v,
            Message::Export => {
                let events = self
                    .filtered_events()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();
                return Task::perform(
                    async move {
                        if let Some(path) = rfd::AsyncFileDialog::new()
                            .set_file_name("活動紀錄.csv")
                            .save_file()
                            .await
                        {
                            activity::export(path.path(), &events)?;
                        }
                        Ok(())
                    },
                    Message::Exported,
                );
            }
            Message::Exported(Err(e)) => self.log(None, e.to_string(), Level::Error),
            Message::Drag => {
                return window::get_latest()
                    .then(|id| id.map(window::drag).unwrap_or_else(Task::none))
            }
            Message::Minimize => {
                return window::get_latest().then(|id| {
                    id.map(|id| window::minimize(id, true))
                        .unwrap_or_else(Task::none)
                })
            }
            Message::Close => {
                if self.active || self.items.iter().any(|i| i.busy) {
                    self.confirm_close = true;
                } else {
                    return iced::exit();
                }
            }
            Message::ConfirmClose => return iced::exit(),
            _ => (),
        }
        let completion = self.finish_and_close();
        if save_state {
            self.persist();
        }
        Task::batch([task, completion])
    }
    fn restore(&mut self) {
        self.restored = true;
        let Some(rt) = self.runtime.as_ref() else {
            return;
        };
        match journal::load(&rt.config.state_file) {
            Ok(Some(j)) => {
                self.batch = j.batch;
                self.core_batch = j.core_batch;
                self.close_started = false;
                for entry in j.entries {
                    let mut i = entry.item;
                    i.busy = false;
                    i.selected = false;
                    if !i.state.terminal() {
                        if i.registration.is_some() {
                            i.state = JobState::Scanning;
                        } else {
                            apply_failure(
                                &mut i,
                                Failure::new(
                                    "復原工作",
                                    "先前上傳結果未知；請由 Core metadata 的 transfer_job_id 查核",
                                    None,
                                ),
                            );
                        }
                    }
                    self.saved_configs.insert(i.id, entry.config.clone());
                    match Runtime::from_config(entry.config) {
                        Ok(rt) => {
                            self.runtimes.insert(i.id, rt);
                        }
                        Err(e) if !i.state.terminal() => {
                            apply_failure(&mut i, e);
                        }
                        Err(_) => (),
                    }
                    self.items.push(i);
                }
                self.active = self
                    .items
                    .iter()
                    .any(|i| i.settings.is_some() && !i.state.terminal());
                self.log(
                    None,
                    format!(
                        "已載入 {} 個先前工作；等待核對 receiver 狀態",
                        self.items.len()
                    ),
                    Level::Info,
                );
            }
            Ok(None) => (),
            Err(e) => {
                self.journal_error = true;
                self.log(None, e.to_string(), Level::Error);
            }
        }
    }
    fn persist(&mut self) {
        if !self.restored {
            return;
        }
        let Some(rt) = &self.runtime else {
            return;
        };
        // Keep a broken existing journal intact for operator recovery.
        if self.journal_error {
            return;
        }
        let journal = journal::Journal {
            version: 1,
            batch: self.batch,
            core_batch: self.core_batch.clone(),
            entries: self
                .items
                .iter()
                .filter_map(|i| {
                    self.saved_configs.get(&i.id).map(|config| journal::Entry {
                        item: i.clone(),
                        config: config.clone(),
                    })
                })
                .collect(),
        };
        if let Err(e) = journal::save(&rt.config.state_file, &journal) {
            self.journal_error = true;
            self.log(None, e.to_string(), Level::Error);
        }
    }
    fn current(&self, id: JobId, attempt: u64) -> bool {
        self.items
            .iter()
            .any(|i| i.id == id && i.attempt == attempt)
    }
    fn ensure_runtime(&mut self, id: JobId) -> Option<Arc<Runtime>> {
        if let Some(rt) = self.runtimes.get(&id) {
            return Some(rt.clone());
        }
        let config = self.saved_configs.get(&id)?.clone();
        match Runtime::from_config(config) {
            Ok(rt) => {
                self.runtimes.insert(id, rt.clone());
                Some(rt)
            }
            Err(e) => {
                self.fail_item(id, e);
                None
            }
        }
    }
    fn fail_item(&mut self, id: JobId, e: Failure) {
        if let Some(i) = self.items.iter_mut().find(|i| i.id == id) {
            apply_failure(i, e.clone());
        }
        self.log(Some(id), e.to_string(), Level::Error);
    }
    fn prune(&mut self) {
        self.runtimes
            .retain(|id, _| self.items.iter().any(|i| i.id == *id));
        self.saved_configs
            .retain(|id, _| self.items.iter().any(|i| i.id == *id));
        if self.items.is_empty() {
            self.first_folder = None;
            self.batch = None;
        }
        self.context = None;
    }
    fn filtered_events(&self) -> Vec<&Activity> {
        self.events
            .iter()
            .filter(|e| {
                e.at.with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
                    .contains(&self.filter_time)
                    && e.filename
                        .to_lowercase()
                        .contains(&self.filter_file.to_lowercase())
                    && e.event
                        .to_lowercase()
                        .contains(&self.filter_event.to_lowercase())
                    && match self.filter_level {
                        Filter::All => true,
                        Filter::Info => e.level == Level::Info,
                        Filter::Warning => e.level == Level::Warning,
                        Filter::Error => e.level == Level::Error,
                    }
            })
            .collect()
    }
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_secs(30)).map(|_| Message::HealthTick),
            iced::time::every(Duration::from_secs(2)).map(|_| Message::PollTick),
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Window(window::Event::FileDropped(path)) => {
                    Some(Message::Dropped(path))
                }
                iced::Event::Window(window::Event::CloseRequested) => Some(Message::Close),
                _ => None,
            }),
        ])
    }
    fn view(&self) -> Element<'_, Message> {
        let title = container(
            row![
                mouse_area(container(text(TITLE).size(20).color(Color::WHITE)).width(Length::Fill))
                    .on_press(Message::Drag),
                button(text("—").color(Color::WHITE))
                    .on_press(Message::Minimize)
                    .style(button::text),
                button(text("×").size(24).color(Color::WHITE))
                    .on_press(Message::Close)
                    .style(button::text)
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        )
        .padding([12, 20])
        .style(|_| container::Style::default().background(NAVY));
        let tabs = row![
            tab_button("檔案傳送", Tab::Transfer, self.tab),
            tab_button("活動紀錄", Tab::Activity, self.tab),
            tab_button("連線設定", Tab::Settings, self.tab),
            Space::with_width(Length::Fill),
            self.health_view("Core", &self.core_health),
            self.health_view("receiver", &self.receiver_health)
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center)
        .padding([10, 20]);
        let mut layout = column![title, tabs];
        if !self.services_ready() {
            let message = if let Some(e) = &self.config_error {
                e.to_string()
            } else if self.core_health.color == HealthColor::Unavailable
                || self.receiver_health.color == HealthColor::Unavailable
            {
                "服務連線失敗，請檢查網路與設定後重試".into()
            } else if self.core_health.failures > 0 || self.receiver_health.failures > 0 {
                "最近一次服務檢查失敗，暫停新傳送，等待再次確認".into()
            } else {
                "正在確認服務與載入設定…".into()
            };
            layout = layout.push(
                container(
                    row![
                        text(message).size(13).width(Length::Fill),
                        button("重試").on_press_maybe((!self.loading).then_some(Message::Reload))
                    ]
                    .spacing(12)
                    .align_y(iced::Alignment::Center),
                )
                .padding([8, 20])
                .width(Length::Fill)
                .style(|_| container::Style::default().background(Color::from_rgb(1.0, 0.93, 0.9))),
            );
        }
        if self.journal_error {
            layout = layout.push(container(text("本機復原紀錄無法讀寫，已停用新批次。請修復紀錄檔或目錄權限後重新開啟程式。").size(13))
                .padding([8,20]).width(Length::Fill).style(|_| container::Style::default().background(Color::from_rgb(1.0,0.93,0.9))));
        }
        if let Some(error) = &self.rules_error {
            layout = layout.push(
                container(
                    row![
                        text(error.to_string()).size(13).width(Length::Fill),
                        button("重試").on_press_maybe((!self.loading).then_some(Message::Reload))
                    ]
                    .spacing(10),
                )
                .padding([8, 20])
                .width(Length::Fill)
                .style(|_| container::Style::default().background(Color::from_rgb(1.0, 0.93, 0.9))),
            );
        }
        let content = match self.tab {
            Tab::Transfer => self.transfer_view(),
            Tab::Activity => self.activity_view(),
            Tab::Settings => self.settings_view(),
        };
        layout = layout.push(container(content).padding([16, 20]).height(Length::Fill));
        let base: Element<'_, Message> = container(layout)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style::default().background(Color::from_rgb(0.96, 0.97, 0.985)))
            .into();
        if self.confirm_clear {
            return modal(
                base,
                "清空檔案清單",
                column![
                    text("清單中仍有待傳送檔案，確定清空？"),
                    row![
                        button("取消").on_press(Message::Dismiss),
                        button("確定清空")
                            .on_press(Message::ConfirmClear)
                            .style(button::danger)
                    ]
                    .spacing(12)
                ]
                .spacing(20)
                .into(),
            );
        }
        if self.confirm_close {
            return modal(
                base,
                "尚有未完成工作",
                column![
                    text("關閉會停止本機上傳與追蹤。receiver 已登記的工作會繼續處理。"),
                    row![
                        button("繼續工作").on_press(Message::Dismiss),
                        button("關閉")
                            .on_press(Message::ConfirmClose)
                            .style(button::danger)
                    ]
                    .spacing(12)
                ]
                .spacing(20)
                .into(),
            );
        }
        if let Some(id) = self.password_target {
            let name = self
                .items
                .iter()
                .find(|i| i.id == id)
                .map(|i| i.filename.as_str())
                .unwrap_or("");
            return modal(
                base,
                "輸入檔案密碼",
                column![
                    text(name),
                    text_input("檔案密碼", &self.file_password.0)
                        .secure(true)
                        .on_input(|s| Message::FilePassword(Secret(zeroize::Zeroizing::new(s)))),
                    row![
                        button("稍後處理").on_press(Message::Dismiss),
                        button("重新掃描").on_press_maybe(
                            (!self.file_password.0.is_empty() && self.services_ready())
                                .then_some(Message::SubmitPassword)
                        )
                    ]
                    .spacing(12)
                ]
                .spacing(16)
                .into(),
            );
        }
        base
    }
    fn health_view<'a>(&self, name: &'a str, health: &Health) -> Element<'a, Message> {
        let color = match health.color {
            HealthColor::Unknown => GREY,
            HealthColor::Healthy => Color::from_rgb(0.1, 0.55, 0.3),
            HealthColor::Unavailable => Color::from_rgb(0.8, 0.15, 0.15),
        };
        let time = health
            .checked_at
            .map(|t| t.with_timezone(&Local).format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "尚未確認".into());
        column![
            text(format!("● {name}")).size(12).color(color),
            text(time).size(10).color(GREY)
        ]
        .spacing(2)
        .into()
    }
    fn transfer_view(&self) -> Element<'_, Message> {
        let editable = !self.active && !self.adding;
        let toolbar = row![
            button("＋ 新增檔案").on_press_maybe(editable.then_some(Message::AddFiles)),
            button("＋ 新增資料夾").on_press_maybe(editable.then_some(Message::AddFolder)),
            Space::with_width(Length::Fill),
            button("移除選取")
                .on_press_maybe(
                    (editable && self.items.iter().any(|i| i.selected)).then_some(Message::Remove)
                )
                .style(button::secondary),
            button("清空清單")
                .on_press_maybe((editable && !self.items.is_empty()).then_some(Message::Clear))
                .style(button::secondary)
        ]
        .spacing(10);
        let rule = pick_list(self.rules.clone(), self.rule.clone(), Message::Rule)
            .placeholder("請選擇掃描規則")
            .width(Length::Fill);
        let dest = pick_list(
            self.destinations.clone(),
            self.destination.clone(),
            Message::Destination,
        )
        .placeholder("請選擇目的地")
        .width(Length::Fill);
        let options: Element<'_, Message> = if self.active {
            let snapshot = self
                .items
                .iter()
                .filter_map(|i| i.settings.as_ref())
                .find(|s| Some(s.batch_id) == self.batch);
            row![
                container(text(format!(
                    "掃描規則：{}",
                    snapshot.map(|s| s.rule.as_str()).unwrap_or("—")
                )))
                .width(Length::Fill),
                container(text(format!(
                    "目的地：{}",
                    snapshot
                        .map(|s| s.destination.label.as_str())
                        .unwrap_or("—")
                )))
                .width(Length::Fill)
            ]
            .spacing(16)
            .into()
        } else {
            row![rule, dest].spacing(16).into()
        };
        let mut name = text_input("批次名稱（留空自動產生）", &self.name);
        let mut password = text_input("批次密碼（選填）", &self.password.0).secure(true);
        if !self.active {
            name = name.on_input(Message::BatchName);
            password =
                password.on_input(|s| Message::BatchPassword(Secret(zeroize::Zeroizing::new(s))));
        }
        let fields = row![
            name.width(Length::FillPortion(2)),
            password.width(Length::FillPortion(1))
        ]
        .spacing(16);
        let header = row![
            text("選取").width(44),
            text("檔案名稱 / 路徑").width(Length::Fill),
            text("大小").width(85),
            text("傳送狀態").width(175)
        ]
        .spacing(8)
        .padding([8, 12]);
        let mut files = column![].spacing(3);
        if self.items.is_empty() {
            files = files.push(
                container(
                    column![
                        text("將檔案或資料夾拖曳至此").size(22).color(GREY),
                        text("或使用上方按鈕加入檔案").size(14).color(GREY)
                    ]
                    .spacing(12),
                )
                .width(Length::Fill)
                .center_x(Length::Fill)
                .padding(60),
            );
        }
        for i in &self.items {
            let id = i.id;
            let mut select = checkbox("", i.selected).width(44);
            if !self.active {
                select = select.on_toggle(move |v| Message::Selected(id, v));
            }
            let label_color =
                if i.state == JobState::Delivered && i.content == Some(ContentKind::Original) {
                    TEAL
                } else {
                    match i.state {
                        JobState::Delivered => Color::from_rgb(0.1, 0.5, 0.28),
                        JobState::Rejected | JobState::Failed => Color::from_rgb(0.75, 0.15, 0.15),
                        _ => BLUE,
                    }
                };
            let status: Element<'_, Message> = if i.state == JobState::NeedsPassword {
                button(text(i.label()).size(13))
                    .on_press_maybe(
                        (self.services_ready() && !i.busy).then_some(Message::Password(id)),
                    )
                    .into()
            } else {
                text(i.label()).color(label_color).size(13).into()
            };
            let file_row = container(
                row![
                    select,
                    column![
                        text(&i.filename).size(14),
                        text(i.path.to_string_lossy().into_owned())
                            .size(10)
                            .color(GREY)
                    ]
                    .width(Length::Fill),
                    text(format_bytes(i.bytes)).size(12).width(85),
                    container(status).width(175)
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            )
            .padding([10, 12])
            .style(|_| container::Style::default().background(Color::WHITE));
            let mut entry = column![mouse_area(file_row).on_right_press(Message::Context(id))];
            if self.context == Some(id) {
                entry = entry.push(
                    button("重送：查詢並恢復原工作")
                        .on_press_maybe(
                            i.retry_enabled(self.services_ready())
                                .then_some(Message::Retry(id)),
                        )
                        .style(button::secondary),
                );
            }
            if let Some(e) = &i.failure {
                entry = entry.push(text(e.to_string()).size(11).color(GREY));
            }
            files = files.push(entry);
        }
        let s = statistics(&self.items, self.batch);
        let progress = if s.total == 0 {
            0.0
        } else {
            (s.delivered + s.rejected + s.failed) as f32 / s.total as f32
        };
        let running = self.active;
        let bar = progress_bar(0.0..=1.0, progress)
            .height(6)
            .style(move |_theme| iced::widget::progress_bar::Style {
                background: Color::from_rgb(0.88, 0.9, 0.93).into(),
                bar: if running { BLUE.into() } else { GREY.into() },
                border: iced::Border::default(),
            });
        let pending = self
            .items
            .iter()
            .filter(|i| i.state == JobState::Pending)
            .count();
        column![
            toolbar,
            options,
            fields,
            header,
            scrollable(files).height(Length::Fill),
            bar,
            row![
                column![
                    text(format!(
                        "{}批次：共 {} 個 / 已送達 {} / 拒絕 {} / 失敗 {} / 未完成 {}",
                        if self.active { "目前" } else { "上一" },
                        s.total,
                        s.delivered,
                        s.rejected,
                        s.failed,
                        s.unfinished
                    ))
                    .size(12),
                    text(format!("⚠ 原始檔 {}（包含在已送達內）", s.original))
                        .size(11)
                        .color(TEAL)
                ]
                .spacing(4)
                .width(Length::Fill),
                button(
                    text(if self.active {
                        format!("傳送中（待傳送 {pending}）")
                    } else {
                        format!("開始傳送（{pending}）")
                    })
                    .size(16)
                )
                .padding([12, 20])
                .on_press_maybe(self.can_start().then_some(Message::Start))
            ]
            .spacing(12)
            .align_y(iced::Alignment::Center)
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    }
    fn activity_view(&self) -> Element<'_, Message> {
        let filters = row![
            text_input("時間 YYYY-MM-DD / HH:MM", &self.filter_time).on_input(Message::TimeFilter),
            text_input("檔案", &self.filter_file).on_input(Message::FileFilter),
            text_input("事件", &self.filter_event).on_input(Message::EventFilter),
            pick_list(
                [Filter::All, Filter::Info, Filter::Warning, Filter::Error],
                Some(self.filter_level),
                Message::LevelFilter
            ),
            button("匯出 CSV").on_press(Message::Export)
        ]
        .spacing(8);
        let mut records = column![].spacing(8);
        for e in self.filtered_events().into_iter().rev() {
            records = records.push(
                container(
                    column![
                        row![
                            text(
                                e.at.with_timezone(&Local)
                                    .format("%Y-%m-%d %H:%M:%S")
                                    .to_string()
                            )
                            .size(11)
                            .color(GREY),
                            text(e.level.to_string()).size(11),
                            text(&e.filename).size(12)
                        ]
                        .spacing(12),
                        text(&e.event).size(13)
                    ]
                    .spacing(5),
                )
                .padding(10)
                .width(Length::Fill)
                .style(|_| container::Style::default().background(Color::WHITE)),
            );
        }
        column![
            filters,
            text(format!("{} 筆活動紀錄", self.filtered_events().len()))
                .size(12)
                .color(GREY),
            scrollable(records).height(Length::Fill)
        ]
        .spacing(16)
        .into()
    }
    fn settings_view(&self) -> Element<'_, Message> {
        let mut settings = column![
            text("服務連線").size(22),
            text(format!("設定檔：{}", self.config_path.display())).size(13)
        ]
        .spacing(18);
        if let Some(rt) = &self.runtime {
            settings = settings.push(text(format!("Core：{}", rt.config.core.url)))
                .push(text(format!("receiver：{}", rt.config.receiver.url)))
                .push(text(format!("分段大小：{}；分段門檻：{}", format_bytes(rt.config.chunk_bytes), format_bytes(rt.config.multipart_threshold))))
                .push(text("Core 綠燈：最近一次 /readyz 健康。\nreceiver 綠燈：/destinations 有效，不代表分享目錄一定可寫。\n每 30 秒檢查，5 秒逾時；連續兩次失敗轉紅，一次成功恢復綠色。").size(14))
                .push(text("URL、CA 與憑證環境變數名稱由外部 TOML 設定；修改後按重新載入。\n正在執行的工作保留啟動時的連線與批次設定。").size(14));
        }
        settings = settings.push(
            button("重新載入設定與檢查服務")
                .on_press_maybe((!self.loading).then_some(Message::Reload)),
        );
        scrollable(settings).into()
    }
}
fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}
fn tab_button(label: &str, tab: Tab, selected: Tab) -> Element<'_, Message> {
    button(text(label).size(15))
        .padding([10, 16])
        .style(if tab == selected {
            button::primary
        } else {
            button::text
        })
        .on_press(Message::Tab(tab))
        .into()
}
fn modal<'a>(
    base: Element<'a, Message>,
    title: &'a str,
    body: Element<'a, Message>,
) -> Element<'a, Message> {
    let base = base.map(|_| Message::Ignored);
    let card = container(column![text(title).size(21), body].spacing(20))
        .padding(28)
        .width(540)
        .style(|_| {
            container::Style::default()
                .background(Color::WHITE)
                .border(iced::Border {
                    radius: 10.0.into(),
                    ..Default::default()
                })
        });
    let overlay = mouse_area(
        container(card)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(|_| {
                container::Style::default().background(Color::from_rgba(0.0, 0.0, 0.0, 0.4))
            }),
    )
    .on_press(Message::Ignored);
    // Opaque mouse area captures background clicks; card controls remain interactive.
    stack![base, overlay].into()
}
fn main() -> iced::Result {
    // Finder does not start the app in the portable package directory.
    // A native bundle entry point must resolve the adjacent config and state there.
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(1).is_none() {
        if let Ok(executable) = std::env::current_exe() {
            if let Some(root) = startup::package_root(&executable) {
                if let Err(error) = std::env::set_current_dir(root) {
                    eprintln!("無法開啟程式設定目錄：{error}");
                }
            }
        }
    }
    #[cfg(target_os = "windows")]
    let font = iced::Font::with_name("Microsoft JhengHei");
    #[cfg(target_os = "macos")]
    let font = iced::Font::with_name("PingFang TC");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let font = iced::Font::with_name("Noto Sans CJK TC");
    iced::application(TITLE, App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::Light)
        .default_font(font)
        .window(window::Settings {
            size: iced::Size::new(900.0, 760.0),
            resizable: false,
            decorations: false,
            exit_on_close_request: false,
            ..Default::default()
        })
        .run_with(App::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (App, JobId, BatchId) {
        let (mut app, _) = App::new();
        let batch = Uuid::new_v4();
        let mut i = Item::new("中文報告.txt".into(), 20).unwrap();
        i.settings = Some(SettingsSnapshot {
            batch_id: batch,
            name: "原批次".into(),
            rule: "原規則".into(),
            destination: Destination {
                key: "original".into(),
                label: "原目的地".into(),
            },
        });
        i.state = JobState::Scanning;
        i.attempt = 2;
        let id = i.id;
        app.items.push(i);
        app.batch = Some(batch);
        app.active = true;
        app.name = "批次名稱".into();
        app.password.replace("batch-secret".into());
        app.active_password = app.password.clone();
        (app, id, batch)
    }
    fn status(id: JobId, batch_id: BatchId, state: JobState) -> JobStatus {
        let mut s = JobStatus::new(RegisterJob {
            id,
            batch_id,
            data_id: "data".into(),
            destination: "original".into(),
            filename: "中文報告.txt".into(),
            sha256: "a".repeat(64),
        });
        s.state = state;
        s
    }
    #[test]
    fn stale_results_and_reordered_rows_cannot_change_another_job() {
        let (mut app, id, batch) = fixture();
        app.items
            .insert(0, Item::new("other.txt".into(), 10).unwrap());
        let _ = app.update(Message::Status(
            id,
            1,
            Ok(status(id, batch, JobState::Delivered)),
        ));
        assert_eq!(app.items[1].state, JobState::Scanning);
        let _ = app.update(Message::Status(
            id,
            2,
            Ok(status(id, batch, JobState::Delivered)),
        ));
        assert_eq!(app.items[1].state, JobState::Delivered);
        assert_eq!(app.items[0].state, JobState::Pending);
        assert_eq!(statistics(&app.items, app.batch).total, 1);
    }
    #[test]
    fn password_wait_is_unfinished_tabs_remain_available_and_settings_are_frozen() {
        let (mut app, id, batch) = fixture();
        let _ = app.update(Message::Status(
            id,
            2,
            Ok(status(id, batch, JobState::NeedsPassword)),
        ));
        let _ = app.update(Message::Tab(Tab::Activity));
        let _ = app.update(Message::BatchName("changed".into()));
        let _ = app.update(Message::Clear);
        assert_eq!(app.tab, Tab::Activity);
        assert!(app.active);
        assert_eq!(app.name, "批次名稱");
        assert!(!app.confirm_clear);
        assert!(!app.password.0.is_empty());
        assert_eq!(app.items[0].settings.as_ref().unwrap().rule, "原規則");
        let _ = app.update(Message::Status(
            id,
            2,
            Ok(status(id, batch, JobState::Rejected)),
        ));
        assert!(!app.active);
        assert!(app.name.is_empty());
        assert!(app.password.0.is_empty());
        assert!(app.active_password.0.is_empty());
        assert!(!app.items[0].retry_enabled(true));
    }
}
