# Aiddiction — план реализации оркестратора проектов агентов

Клиент-серверное приложение: сервер на Rust управляет проектами (директориями
на своей машине) и запускает в них ACP-агентов отдельными процессами; клиент на
Rust/GPUI (`gpui-kit`) — десктоп и Android — подключается к одному из
настроенных серверов, показывает проекты, создаёт новые (пустые или из git),
запускает в проекте агента и работает с ним как с чатом. ACP проксируется
сервером клиенту через WebSocket.

Документ — рабочий план: архитектура, протоколы, структура крейтов, этапы с
критериями готовности и риски. Все решения, которые берутся из референсов,
снабжены ссылкой на источник.

## 0. Референсы и что из них берём

| Референс | Где | Что берём |
|---|---|---|
| multiagent-ui | github.com/EvgeniiDev/multiagent-ui, `deploy/acp-ws-bridge.mjs` | Модель «stdio-агент ↔ WebSocket»: **одна строка NDJSON = один WS-фрейм**; переименование id запросов per-socket; маршрутизация `session/update` к сокету-владельцу сессии; watchdog по тишине на проводе вместо фиксированного таймаута; ping/pong и отсечение полуоткрытых сокетов; лимит буфера отправки; `POST /api/fs/mkdir` в пределах корня. Проект = просто директория. |
| Zed | `~/.cargo/git/checkouts/zed-*/b5e03c7/crates/agent_servers/src/acp.rs`, `crates/acp_thread` | Клиент ACP на крейте `agent-client-protocol = "=1.1.0"` (`features = ["unstable"]`): транспорт `Lines::new(sink, stream)`, `Client.builder().on_receive_request(...).connect_with(...)`, перенос обработчиков (они `Send`) на главный поток GPUI через `mpsc`-очередь `ForegroundWork`; `initialize` в гонке со статусом процесса; модель треда `AcpThread` (`AgentThreadEntry`: UserMessage / AssistantMessage / ToolCall, Plan, права, `handle_session_update`). |
| money-manager-ng | `~/money-manager-ng` | Структура «core-крейт без UI + app-крейт `lib` + `cdylib`», `android_main` (защита от повторного старта, `android_logger`, XDG → `internal_data_path`), `gpui-mobile` (`gpui-pre-mobile`, pinned rev), Gradle-проект с `NativeActivity`, `build-aux/build-android.sh`, профили `release`/`dist`, `Settings` как GPUI global, адаптивный layout по ширине в rem, скриптовый режим и UI-тесты под headless sway. |
| matterfast (`~/matras`) | `crates/matterfast/src/runtime.rs`, `android/` | Мост GPUI ↔ Tokio (`runtime::spawn(fut, |res, cx| …)`, `runtime::receive` для потоков), `reqwest` + `rustls-platform-verifier` на Android, тест-сервер-заглушка как отдельный крейт, `DEMO`-режим. |
| gpui-kit 0.7 | skill `gpui-kit`, `gpui-component-0.7.0/src` | Компоненты: `sidebar`, `list`/`searchable_list`, `input`, `dialog`, `sheet`, `form`, `select`, `tab`, `text` (markdown `TextView`), `message`, `message_scroller`, `bubble`, `collapsible`, `notification`, `spinner`, `questionnaire`. Перед кодом — Coding Guides и Design Guides из skill. |

## 1. Цели и не-цели

**MVP должен уметь:**

1. Клиент: список серверов (имя, URL, токен), добавить/изменить/удалить,
   выбрать активный, проверить соединение.
2. Клиент: список проектов выбранного сервера, поиск, статус агентов в проекте.
3. Клиент: создать проект — пустую директорию или `git clone` (URL, ветка,
   опционально токен) с прогрессом клонирования.
4. Клиент: выбрать агента из каталога, **который приходит с сервера**
   (по умолчанию Claude Code и Codex; плюс агенты из конфига сервера и
   произвольные, добавленные из клиента командой bash, как в Zed), запустить его в проекте,
   работать в чате: промпт, стриминг ответа, размышления, tool calls, план,
   запросы прав, отмена, slash-команды, смена режима/модели.
5. Сервер: REST API для серверной информации, каталога агентов, проектов и
   экземпляров агентов; WebSocket-прокси ACP; WebSocket событий.
6. Сборка клиента под Linux (основная) и Android arm64 (APK).

**Не входит в MVP (позже):** несколько пользователей с ролями, доска задач и
оркестрация «агент управляет агентами» из multiagent-ui, учёт токенов/стоимости,
голосовой ввод, редактор файлов, просмотр git-диффа проекта, iOS/macOS/Windows-
сборки клиента, контейнерная изоляция агентов.

## 2. Архитектура

```
┌──────────────────────────────┐            ┌───────────────────────────────────────────┐
│ Клиент aiddiction (GPUI)      │  HTTPS     │ aiddiction-server (axum + tokio)           │
│                              │  REST      │                                           │
│ Settings (серверы)           ├───────────►│ /api/v1/...  проекты, каталог, инстансы   │
│ ServerConnection ── events ◄─┼──── WS ────┤ /api/v1/events   (статусы, прогресс)      │
│ AgentSession (ACP Client) ◄──┼──── WS ────┤ /api/v1/instances/{id}/acp                │
│   AcpThread-модель           │  NDJSON    │   AcpProxy ──stdio── агент (процесс)      │
└──────────────────────────────┘            │                       cwd = projects/<id> │
                                            │ SQLite: проекты, инстансы, git-задачи     │
                                            └───────────────────────────────────────────┘
```

Ключевое решение: **сервер — полноправный ACP-клиент агента, а не прозрачная
труба.** Агент работает с файлами на сервере, клиент (особенно телефон) их не
видит. Поэтому:

- `initialize` к агенту делает сервер один раз при запуске процесса и
  объявляет `fs.readTextFile/writeTextFile` и `terminal` — эти запросы агента
  сервер обслуживает сам, в пределах директории проекта.
- `initialize` от клиента сервер не пересылает, а отвечает кешированным ответом
  агента (`agentCapabilities`, `authMethods`, `agentInfo`) — так переподключение
  клиента (Android убивает сокеты в фоне) не требует перезапуска агента.
- `session/new`, `session/load`, `session/resume`: сервер **переписывает
  `cwd`** на путь проекта (клиент передаёт любой, хоть `"."`) и запрещает
  `additionalDirectories` вне проекта.
- `session/request_permission`, `session/elicitation` и прочие
  запросы агента, которые требуют человека, идут клиенту-владельцу сессии.
- Всё остальное (включая неизвестные/нестабильные методы `_…`) прокидывается
  как есть, без парсинга в типы — прокси работает на `serde_json::Value` и
  разбирает только то, что перехватывает. Это делает его устойчивым к
  расширениям протокола (Zed использует `unstable`-фичи).

## 3. Структура репозитория

```
aiddiction/
├── Cargo.toml                    # workspace, [workspace.package] version, профили dev/release/dist
├── rust-toolchain.toml           # channel = "1.95" (как в money-manager-ng)
├── CLAUDE.md                     # команды, архитектура, правила (заводится на этапе 1)
├── crates/
│   ├── aiddiction-api/            # общие типы REST/событий (serde), без tokio/gpui
│   ├── aiddiction-server/         # бинарь сервера
│   ├── aiddiction-client/         # HTTP/WS-клиент сервера + ACP-транспорт; без gpui
│   ├── aiddiction/                # приложение: lib + cdylib + тонкий main.rs
│   └── aiddiction-fake-agent/     # ACP-агент-заглушка со сценариями (тесты, демо)
├── android/                      # Gradle-проект из money-manager-ng/android
├── build-aux/
│   ├── build-android.sh
│   ├── ui-tests.sh
│   └── check-version.sh
└── docs/
    ├── PLAN.md                   # этот файл
    └── API.md                    # спецификация REST/WS (заводится на этапе 2)
```

Правило слоёв (как в matterfast/money-manager): `aiddiction-api` и
`aiddiction-client` не зависят от GPUI и **никогда** не начинают зависеть.
`aiddiction-server` не зависит от `aiddiction-client`. Приложение зависит от
`gpui-kit` и `aiddiction-client`.

### Workspace `Cargo.toml` (скелет)

```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.95"
license = "GPL-3.0"
authors = ["Akergez"]

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
anyhow = "1"
tracing = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "net", "process", "io-util", "fs"] }
futures = "0.3"
uuid = { version = "1", features = ["v7", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
agent-client-protocol = { version = "=1.1.0", features = ["unstable"] }  # та же, что у Zed

[profile.dev]
debug = "line-tables-only"
[profile.dev.package."*"]
opt-level = 2            # GPUI без оптимизаций рисует медленно — урок money-manager
[profile.release]
incremental = true
[profile.dist]
inherits = "release"
lto = "thin"
codegen-units = 1
incremental = false
strip = true
```

## 4. Сервер (`aiddiction-server`)

### 4.1 Стек

- `axum` 0.8 (+ `ws`), `tokio`, `tower-http` (trace, limit, cors не нужен —
  клиент нативный).
- TLS: `axum-server` с `rustls` (опционально; по умолчанию слушаем
  `127.0.0.1` и рекомендуем reverse-proxy/Tailscale для внешнего доступа).
- `rusqlite` (`bundled`) за `tokio::task::spawn_blocking` или `tokio-rusqlite`.
- `tokio::process` для агентов и `git`.
- `clap` для CLI, `toml` для конфига, `tracing-subscriber`.

### 4.2 Конфигурация и данные

`$XDG_CONFIG_HOME/aiddiction-server/config.toml`:

```toml
listen = "127.0.0.1:7878"
projects_root = "~/aiddiction-projects"   # все проекты — поддиректории
# tls = { cert = "...", key = "..." }

[limits]
max_running_agents = 8
idle_stop_minutes = 60        # агент без подключённых клиентов и без активного prompt'а
acp_idle_timeout_secs = 180   # watchdog «тишины на проводе», как в acp-ws-bridge

# Встроены и не требуют настройки:
#   claude = npx -y @agentclientprotocol/claude-agent-acp
#   codex  = npx -y @agentclientprotocol/codex-acp
# (те же ACP-адаптеры, что использует Zed; проверено 2026-10-24 по npm).
# Запись с тем же id заменяет встроенного агента, новая — добавляется.
[[agents]]
id = "gemini"
name = "Gemini CLI"
command = "gemini"            # строка для bash: `npx -y pkg`, `FOO=1 agent` — всё можно
args = ["--experimental-acp"]
env = { }
```

Каталог агентов собирается из трёх источников (`AgentSource` в API):
`builtin` (Claude, Codex), `config` (`[[agents]]` из файла, только чтение
через API) и `custom` — добавленные клиентами через `POST /agents`, хранятся
в `$XDG_DATA_HOME/aiddiction-server/agents.json`. Форма описания агента — как
у Zed в `agent_servers`: `command`, `args`, `env`. `command` выполняется
`bash -c '<command> "$@"'` в директории проекта, аргументы идут после него.
Клиент получает каталог с командами (чтобы человек видел, что запустится на
сервере); значения env у встроенных агентов и агентов из конфига заменяются
на `•••` — там могут быть API-ключи.

Токен доступа: при первом запуске генерируется случайный (32 байта, base64url),
хранится как хеш (argon2/sha256) в `$XDG_DATA_HOME/aiddiction-server/auth`,
сам токен печатается в консоль один раз (`aiddiction-server token --rotate`
выпускает новый). Секреты агентов (API-ключи) — только в `env` конфига или в
окружении сервера, клиенту никогда не отдаются.

`$XDG_DATA_HOME/aiddiction-server/state.sqlite` — таблицы:

- `projects(id TEXT PK uuid-v7, name, dir_name UNIQUE, origin_kind 'empty'|'git', git_url, git_branch, created_at, last_opened_at)`
- `git_jobs(id, project_id, state 'running'|'done'|'failed', progress, message, started_at, finished_at)`
- `instances(id, project_id, agent_id, state, pid, started_at, stopped_at, exit_status)` — история; живое состояние в памяти.
- `sessions(session_id, instance_id, project_id, title, created_at, updated_at)` —
  для списка сессий, если агент не поддерживает `session/list`.

Миграции — встроенные SQL-скрипты с `PRAGMA user_version`.

### 4.3 REST API (`/api/v1`, JSON, `Authorization: Bearer <token>`)

Типы — в `aiddiction-api`, сериализуются `serde` с `rename_all = "camelCase"`.
Ошибки — `{ "error": { "code": "project_not_found", "message": "…" } }` с
корректным HTTP-статусом.

| Метод | Путь | Назначение |
|---|---|---|
| GET | `/health` | Без авторизации. `{ ok, version }` — для «проверить соединение». |
| GET | `/server` | `{ name, version, apiVersion, projectsRoot, limits }` |
| GET | `/agents` | Каталог: `[{ id, name, source: builtin\|config\|custom, command: { command, args, env } }]`. |
| POST | `/agents` | Добавить custom-агента: `{ name, command, args?, env? }` → `201 { agent }` (id — slug имени). |
| PUT / DELETE | `/agents/{id}` | Изменить / удалить custom-агента; встроенные и из конфига → `403 agent_read_only`. |
| GET | `/projects` | Список: `[{ id, name, origin, createdAt, lastOpenedAt, instances: [{id, agentId, state}] }]` |
| POST | `/projects` | Создать: `{ name, source: { kind: "empty" } \| { kind: "existing", path } \| { kind: "git", url, branch?, token? } }` → `201 { project, gitJobId? }`. Для git проект в состоянии `cloning`. |
| GET | `/projects/{id}` | Детали + `git` (текущая ветка, `HEAD`, грязность — `git status --porcelain=v2 -b`). |
| PATCH | `/projects/{id}` | Переименовать (только отображаемое имя). |
| GET | `/fs/dirs?path=…&hidden=` | Папки в папке сервера (по умолчанию — домашней) для выбора существующей папки проекта. |
| DELETE | `/projects/{id}?deleteFiles=true` | Остановить агентов; удалить запись; директорию — только с флагом. |
| GET | `/git-jobs/{id}` | Состояние клонирования. |
| GET | `/projects/{id}/sessions` | Сессии проекта (из `session/list` агента, иначе из таблицы). |
| POST | `/projects/{id}/instances` | `{ agentId }` → запустить агента → `201 { instance }` после успешного `initialize`. Если такой агент в проекте уже запущен — `200` с существующим (один инстанс агента на проект). |
| GET | `/instances/{id}` | `{ id, projectId, agentId, state, startedAt, agentInfo, capabilities, stderrTail }` |
| DELETE | `/instances/{id}` | Остановить (SIGTERM группе процессов, через 5 c SIGKILL). |
| GET (WS) | `/instances/{id}/acp` | ACP-прокси, см. 4.5. |
| GET (WS) | `/events` | Поток событий сервера, см. 4.6. |

Имя директории проекта: `slug(name)` + суффикс при коллизии; путь всегда
`projects_root/dir_name`, проверяется канонизацией (никаких `..`, симлинков
наружу). Клиент путей не присылает вовсе.

### 4.4 Менеджер агентов (`agents/`)

```rust
pub struct AgentSupervisor {          // один на сервер, Arc
    catalog: Arc<Catalog>,
    instances: Mutex<HashMap<InstanceId, Arc<Instance>>>,
    events: broadcast::Sender<ServerEvent>,
}

pub struct Instance {
    id: InstanceId,
    project: ProjectRef,              // id + канонический путь
    agent_id: AgentId,
    state: watch::Sender<InstanceState>,   // Starting | Ready | Exited{status} | Failed{msg}
    proxy: AcpProxy,                  // см. 4.5
    stderr_tail: Mutex<VecDeque<String>>,  // последние ~200 строк — для диагностики в клиенте
}
```

Запуск:

1. `Command::new(cmd).args(..).envs(..).current_dir(project_dir)`,
   `stdin/stdout/stderr = piped`, `kill_on_drop(true)`, на Unix —
   `process_group(0)`, чтобы при остановке убивать всё дерево (`npx` → node →
   инструменты агента).
2. Окружение очищается до белого списка (`PATH`, `HOME`, `LANG`, `TERM`…) +
   `env` из конфига агента. Токен сервера в окружение агента не попадает.
3. Задачи: чтение stdout построчно (`BufReader::lines`, без ограничения 64 КиБ —
   у Pi `session/new` весит ~113 КБ, см. комментарий в acp-ws-bridge), запись
   в stdin через `mpsc` с добавлением `\n`, stderr → `stderr_tail` + `tracing`.
4. `initialize` (`protocolVersion: 1`, `clientCapabilities: { fs: {readTextFile, writeTextFile}, terminal: true }`,
   `clientInfo: { name: "aiddiction-server", version }`) в гонке с
   `child.wait()` — как в Zed: если процесс умер раньше ответа, ошибка запуска
   содержит хвост stderr.
5. Состояние `Ready`, событие `instance.state` в `/events`.
6. Остановка: по `DELETE`, по `idle_stop_minutes`, при удалении проекта, при
   завершении сервера (graceful shutdown: SIGTERM всем группам, ждать 5 c).
7. Падение процесса: все ожидающие запросы клиентов получают JSON-RPC ошибку
   `"agent exited (code=…)"`, сокеты закрываются кодом 1011, событие в
   `/events`. Автоперезапуска нет — клиент предлагает «Перезапустить».

### 4.5 ACP-прокси (`acp_proxy/`)

Работает на сырых JSON-RPC сообщениях (`serde_json::Value`). Проводной формат
к клиенту: **WebSocket text-фрейм = ровно одно JSON-RPC сообщение** (без `\n`).

Состояние прокси на инстанс:

```rust
struct ProxyState {
    next_upstream_id: u64,
    // client → agent запросы: upstream id → (conn, исходный id, method, sessionId?)
    pending_client: HashMap<u64, PendingClientRequest>,
    // agent → client запросы, ждущие ответа человека: upstream id агента → (sessionId?, raw frame)
    pending_agent: HashMap<JsonRpcId, PendingAgentRequest>,
    // sessionId → conn, последним вызвавший session/new|load|resume|prompt
    session_owner: HashMap<SessionId, ConnId>,
    // кольцевой журнал уведомлений на сессию для доигрывания после переподключения
    journal: HashMap<SessionId, SessionJournal>,
    init_response: Value,               // кеш ответа агента на initialize
    connections: HashMap<ConnId, mpsc::Sender<Message>>,
    last_traffic: Instant,
}
```

Правила (основа — `acp-ws-bridge.mjs`, адаптировано):

| Направление | Сообщение | Действие |
|---|---|---|
| C→A | `initialize` | Не пересылать. Ответить `init_response` с подменой `id`. |
| C→A | `authenticate` | Переслать (агенту может понадобиться логин); при успехе пометить инстанс как аутентифицированный. |
| C→A | `session/new`, `session/load`, `session/resume` | Переписать `params.cwd` на путь проекта, отфильтровать `additionalDirectories`, `mcpServers` пропустить как есть (позже — серверный список MCP). Владелец сессии = это соединение. |
| C→A | любой запрос | Подменить `id` на уникальный `upstream id`, запомнить исходный; ответ вернуть **только** спросившему соединению с восстановленным `id`. |
| C→A | `session/prompt` | Как выше + записать `sessionId` в журнал как «prompt in flight». Если на эту сессию уже идёт `session/load` от другого соединения — придержать load до конца prompt'а (баг Devin из multiagent-ui). |
| C→A | `session/cancel` (уведомление) | Переслать. |
| C→A | ответ на запрос агента | Найти в `pending_agent`, переслать агенту, удалить. Первый ответ выигрывает, остальным соединениям разослать `$/aiddiction/agent_request_resolved`. |
| A→C | `fs/read_text_file`, `fs/write_text_file` | **Обслужить на сервере**: путь канонизировать, требовать нахождения внутри проекта, `line`/`limit` для чтения, запись через temp-файл + rename. Клиенту не отправлять. |
| A→C | `terminal/*` | **Обслужить на сервере** (`TerminalManager`: процесс в cwd проекта, буфер вывода с `outputByteLimit`, `wait_for_exit`, `kill`, `release`). Клиенту не отправлять; вывод терминала клиент видит через `ToolCallContent::Terminal` — поэтому сервер дополнительно шлёт клиенту расширение `$/aiddiction/terminal_output { terminalId, chunk }` (Zed использует аналогичный `terminal_output` в `_meta`). |
| A→C | `session/request_permission`, `session/elicitation`, прочие запросы | Владельцу сессии, если он подключён; иначе всем соединениям инстанса; иначе держать в `pending_agent` и отдать первому подключившемуся. |
| A→C | `session/update` и другие уведомления с `sessionId` | Записать в журнал сессии, отправить владельцу (если нет — всем, кто «смотрит» сессию). |
| A→C | без `sessionId` | Всем соединениям. |

**Журнал и переподключение.** Клиент на телефоне теряет сокет постоянно.
Каждое уведомление получает монотонный `seq` в пределах сессии, прокси держит
последние N (например, 5000 сообщений / 8 МБ) в памяти. При подключении клиент
шлёт расширение `$/aiddiction/attach { sessionId, afterSeq }` — прокси доигрывает
хвост журнала (каждое сообщение оборачивается в
`_meta.aiddiction.seq`), отправляет висящие `pending_agent` запросы этой сессии
и делает соединение владельцем. Если хвост уже вытеснен — ответ
`{ truncated: true }`, клиент делает обычный `session/load` (агент проиграет
историю целиком). Сервер не хранит журнал на диске в MVP.

**Watchdog.** Если есть ожидающие запросы клиента и на проводе тишина дольше
`acp_idle_timeout_secs`, а агент не ждёт ответа клиента (`pending_agent`
пуст) — ответить на ожидающие ошибкой `"no traffic for Ns"`. Отдельной
ошибкой отличается `"agent exited"`.

**WebSocket-гигиена.** Ping каждые 30 c, соединение без входящих фреймов за
два интервала закрывается; `max_message_size` ≥ 16 МБ; ограниченная очередь
отправки на соединение (при переполнении — закрыть соединение, клиент
переподключится и доиграет из журнала, а не потеряет сообщения молча).

### 4.6 События (`/api/v1/events`)

WS, сервер → клиент, JSON `{ "type": "...", ... }`:

- `project.created | project.updated | project.deleted`
- `git_job.progress { jobId, projectId, phase, percent, message }` и
  `git_job.finished { jobId, ok, error? }`
- `instance.state { instanceId, projectId, agentId, state, exitStatus? }`
- `session.activity { instanceId, sessionId, busy }` — для индикатора «агент
  работает» в списке проектов.

При подключении первым сообщением идёт `snapshot` (проекты + инстансы), чтобы
клиенту не нужно было синхронизировать REST и поток вручную.

### 4.7 Git

Через системный `git` (поддерживает ключи/credential helper пользователя
сервера, LFS, всё, что настроено), а не `gix`/`git2`:

- `git clone --progress [--branch B] [--single-branch] URL <dir>.tmp-<job>`;
  stderr парсится по `\r` на фазы (`Receiving objects: 42% …`) → `git_job.progress`.
- Токен HTTPS не попадает в argv и в `.git/config`: передаётся через
  `GIT_ASKPASS`-скрипт во временном файле `0700` + переменные окружения,
  `GIT_TERMINAL_PROMPT=0`.
- По успеху — `rename` в итоговую директорию, иначе удалить временную.
- Таймаут неактивности (нет вывода 5 минут) → отмена.
- URL принимаются `https://`, `ssh://`, `git@host:path`; `file://` и
  локальные пути запрещены.

### 4.8 Безопасность

- Bearer-токен на всё, кроме `/health`; сравнение за постоянное время.
- По умолчанию bind на `127.0.0.1`; при bind на внешний адрес без TLS — громкое
  предупреждение в логе.
- Все пути проекта — от `projects_root`, канонизация и проверка префикса;
  fs/terminal-запросы агента ограничены директорией проекта.
- Агенты работают с правами пользователя сервера — это граница доверия; в
  README прямо сказано, что сервер стоит запускать от отдельного пользователя
  или в контейнере (как multiagent-ui с режимами docker/host).
- Лимиты тел запросов, размера WS-сообщений, числа агентов.

### 4.9 Модули сервера

```
crates/aiddiction-server/src/
├── main.rs            # CLI (serve | token --rotate | check-config), tracing, graceful shutdown
├── config.rs          # Config, Catalog, загрузка/валидация
├── auth.rs            # токен, middleware
├── db.rs              # пул/миграции, репозитории Projects/Instances/Sessions/GitJobs
├── projects.rs        # создание/удаление директорий, slug, канонизация
├── git.rs             # clone с прогрессом, askpass, status
├── events.rs          # broadcast ServerEvent, WS-хендлер /events
├── agents/
│   ├── mod.rs         # AgentSupervisor
│   ├── process.rs     # spawn, process group, stderr tail, stop
│   └── terminal.rs    # TerminalManager для terminal/*
├── acp_proxy/
│   ├── mod.rs         # AcpProxy: цикл upstream, attach/detach соединений
│   ├── routing.rs     # таблицы id/владельцев, правила из 4.5
│   ├── journal.rs     # SessionJournal (seq, кольцо, лимиты)
│   ├── fs.rs          # обслуживание fs/* в пределах проекта
│   └── watchdog.rs
└── http/
    ├── mod.rs         # Router
    ├── projects.rs
    ├── instances.rs   # + WS /acp
    └── agents.rs
```

`routing.rs` — чистая логика без ввода-вывода (вход: сообщение + источник,
выход: список действий «отправить туда-то»), чтобы покрыть её юнит-тестами
таблично.

## 5. Общие крейты

### 5.1 `aiddiction-api`

DTO REST и событий, коды ошибок, `API_VERSION`, имена расширений ACP
(`$/aiddiction/attach`, `$/aiddiction/terminal_output`,
`$/aiddiction/agent_request_resolved`) и их параметры. Только `serde`,
`uuid`, `chrono`.

### 5.2 `aiddiction-client` (без GPUI)

- `ServerClient` поверх `reqwest` (`rustls-tls`, без native-tls — проще для
  Android; на Android — `rustls-platform-verifier`, как в matterfast).
  Методы 1:1 с REST-таблицей.
- `EventStream` — `tokio-tungstenite` (`rustls`), автопереподключение с
  экспоненциальной задержкой, отдаёт `Stream<Item = ServerEvent>`; после
  переподключения первым приходит `snapshot`.
- `AcpSocket` — открыть `/instances/{id}/acp`, вернуть пару
  `(Sink<String>, Stream<io::Result<String>>)` для `agent_client_protocol::Lines`.
  Переподключение здесь **не** делается: обрыв = конец ACP-соединения,
  восстановление — уровнем выше (см. 6.5).

## 6. Клиент (`aiddiction`)

### 6.1 Каркас (как money-manager-ng)

```
crates/aiddiction/src/
├── main.rs            # fn main() { aiddiction::run() }
├── lib.rs             # run(), android_main(), start(cx)
├── runtime.rs         # мост GPUI ↔ Tokio — копия подхода matterfast/runtime.rs
├── paths.rs           # XDG-пути (на Android указывают в internal_data_path)
├── settings.rs        # Settings: GPUI global над settings.json
├── assets.rs
├── shell/             # Shell: корневой view, Layout (phone/rail/full) по ширине в rem
├── servers/           # экран и диалоги серверов
├── projects/          # список проектов, NewProjectDialog
├── session/           # AgentSession (ACP), Thread-модель
├── chat/              # отрисовка треда, композер, tool calls, permissions
├── script.rs          # AIDDICTION_SCRIPT для UI-тестов
└── demo.rs            # AIDDICTION_DEMO: fake-сервер в процессе
```

Зависимости приложения: `gpui-kit = "0.7.0"`, `aiddiction-client`,
`aiddiction-api`, `agent-client-protocol`, `tokio`, `futures`,
`async-channel`, `serde`, `serde_json`, `dirs`, `tracing`,
`tracing-subscriber`, `keyring` (только desktop; на Android токен хранится в
приватной директории), Android-блок как в money-manager-ng (см. раздел 7).

### 6.2 Состояние

| Сущность | Тип | Владеет |
|---|---|---|
| `Settings` | GPUI global | Список серверов `{ id, name, url, token_ref }`, активный сервер, тема, размер UI. Пишется в `settings.json` на каждый `Settings::update`. |
| `ServerConnection` | `Entity` | `ServerClient`, поток событий, кеш `projects`, `agents`, `instances`, статус соединения. Один на активный сервер; при смене сервера пересоздаётся. Все REST-вызовы — через `runtime::spawn`, результат применяется на главном потоке и `cx.notify()`. |
| `AgentSession` | `Entity` | ACP-соединение к одному инстансу, `Thread` текущей сессии, очередь запросов прав, `available_commands`, режимы/модели, состояние `Connecting / Ready / Reconnecting / Closed{reason}`. |
| `Thread` | часть `AgentSession` | Упрощённый `AcpThread` из Zed: `Vec<ThreadEntry>` с `UserMessage`, `AssistantMessage { chunks: Vec<Text \| Thought> }`, `ToolCall { id, title, kind, status, content: Vec<Content \| Diff \| Terminal>, locations }`, `Plan`, `last_seq`. |
| `Workspace` | `Entity` | Что открыто: экран, выбранный проект, открытые сессии (вкладки на десктопе, стек на телефоне). Каждая команда — один метод, вызываемый и кнопкой, и шорткатом (правило money-manager). |

Ничто, кроме `ServerConnection`, не ходит в REST; ничто, кроме
`AgentSession`, не трогает ACP. Tokio-задачи не касаются сущностей —
только через `runtime::spawn`/`runtime::receive`.

### 6.3 ACP-клиент поверх WebSocket

Повторяет `agent_servers/src/acp.rs` из Zed, заменив процесс на сокет:

```rust
// session/connection.rs — эскиз; сигнатуры сверить с исходниками
// agent-client-protocol 1.1.0 и Zed, ничего не придумывать.
let (sink, stream) = acp_socket.split_lines();            // aiddiction-client
let transport = agent_client_protocol::Lines::new(sink, stream);
let (dispatch_tx, dispatch_rx) = futures::channel::mpsc::unbounded::<ForegroundWork>();
let (conn_tx, conn_rx) = futures::channel::oneshot::channel();

let io = connect_client_future("aiddiction", transport, dispatch_tx, conn_tx);
runtime::runtime().spawn(io);                              // транспорт живёт на Tokio

// главный поток GPUI: разбор очереди ForegroundWork → AgentSession
cx.spawn(async move |this, cx| {
    while let Some(work) = dispatch_rx.next().await { work.run(&this, cx); }
});
```

Обработчики, которые регистрирует клиент: `session/request_permission`,
`session/elicitation` (форма → `questionnaire`), уведомления
`session/update`, расширения `$/aiddiction/*`. Обработчики
`fs/*` и `terminal/*` **не** регистрируются и в `clientCapabilities` не
объявляются — их обслуживает сервер (клиентский `initialize` всё равно
отвечается сервером из кеша).

Порядок открытия чата:

1. `POST /projects/{id}/instances { agentId }` (или существующий инстанс).
2. Открыть `/instances/{id}/acp`, `initialize` (ответ из кеша сервера).
3. Если `authMethods` не пуст и агент требует логина — показать выбор метода,
   `authenticate`. (Терминальная авторизация, как в Zed, в MVP не
   поддерживается: такой агент нужно залогинить на сервере вручную; клиент
   показывает понятное сообщение.)
4. Новая сессия: `session/new { cwd: "." , mcpServers: [] }`. Существующая —
   `$/aiddiction/attach { sessionId, afterSeq: 0 }`, при `truncated` или
   отсутствии в журнале — `session/load`, если `agentCapabilities.loadSession`.
5. Промпт: `session/prompt { sessionId, prompt: [ContentBlock::Text, …] }`;
   пока он не завершился — композер показывает «Стоп» (`session/cancel`).

### 6.4 Отрисовка чата

- `message_scroller` + `message`/`bubble` для сообщений; markdown —
  `TextView` из `gpui_kit::component::text`; размышления —
  `collapsible`, свёрнуты по умолчанию.
- Tool call — карточка: иконка по `kind`, заголовок, статус
  (`pending/in_progress/completed/failed`, `spinner`), раскрываемое
  содержимое: текст/markdown, diff (MVP: построчный unified-diff своими
  силами по `oldText/newText`; позже — подсветка), терминал (моноширинный
  блок, хвост из `$/aiddiction/terminal_output`).
- Plan — список с чекбоксами-статусами над композером.
- Запрос прав — инлайн-карточка в треде у соответствующего tool call с
  кнопками из `options` (`allow_once`, `allow_always`, `reject_once`,
  `reject_always`) и бейдж «нужно действие» на вкладке/проекте. На телефоне —
  `sheet` снизу.
- Композер: `Input` многострочный, Enter — отправить, Shift+Enter — перенос
  (на телефоне — кнопка), `/` открывает список `available_commands`
  (`select`/`combobox`), селекторы режима (`session/set_mode`) и модели
  (`session/set_model` / config options), если агент их объявил.
- Стабильные `ElementId` из доменных id (`tool_call_id`, индекс entry
  только внутри сообщения) — требование Coding Guides gpui-kit.
- Длинные треды: виртуализация (`list`/`VirtualList`), иначе рендер
  тормозит — проверить через `frames:` в скриптовом режиме.

### 6.5 Переподключение и жизненный цикл

- `AgentSession` хранит `last_seq` по сессии. При обрыве сокета — состояние
  `Reconnecting`, повтор с задержкой 0.5 → 1 → 2 → … → 15 c, затем снова
  `initialize` + `$/aiddiction/attach { afterSeq: last_seq }`. Уведомления
  с `seq ≤ last_seq` игнорируются (идемпотентность).
- Ожидающий ответ `session/prompt` после обрыва считается потерянным для
  клиента, но не для агента: итог хода клиент узнаёт из доигранного журнала
  (сервер пишет в журнал и синтетическое `$/aiddiction/prompt_finished
  { stopReason }`, когда агент отвечает на prompt).
- На Android при уходе в фон сокеты не держим насильно; при возвращении
  (`on_resume`/фокус окна) — немедленное переподключение.
- Событие `instance.state = Exited` из `/events` → чат показывает плашку
  «Агент завершился» + хвост stderr из `GET /instances/{id}` + кнопку
  «Перезапустить».

### 6.6 Экраны и навигация

Layout как в money-manager-ng (`shell/layout.rs`): ширина < 44 rem — телефон
(стек страниц, кнопка «назад»), < 66 rem — рейл иконок, иначе — полный
сайдбар.

1. **Серверы** (`servers/`): список (`list`), у каждого — статус проверки
   `/health`; диалог добавления/редактирования (`form`: имя, URL, токен,
   кнопка «Проверить»). Выбор сервера делает его активным. Первый запуск без
   серверов открывает этот экран.
2. **Проекты** (`projects/`): `searchable_list` проектов активного сервера с
   бейджами запущенных агентов и «нужно действие»; переключатель сервера в
   шапке сайдбара (`select`). Кнопка «Новый проект».
3. **Новый проект** (`dialog`, на телефоне — полноэкранный `sheet`):
   `tab`/radio «Пустой | Из git»; имя; для git — URL, ветка, токен
   (необязательно). После создания git-проекта — карточка прогресса в списке
   (`progress`) из `git_job.progress`.
4. **Проект**: заголовок, git-инфо (ветка, грязность), список сессий
   (`GET /projects/{id}/sessions`), выбор агента (`select` по `GET /agents`),
   «Новый чат», запущенные инстансы с кнопкой «Остановить».
5. **Чат** (`chat/`): см. 6.4. Десктоп — вкладки сессий справа от
   сайдбара; телефон — отдельная страница стека.
6. **Настройки**: тема, размер интерфейса, о программе.

### 6.7 Хранение настроек клиента

`$XDG_CONFIG_HOME/aiddiction/settings.json`:
`{ servers: [{ id, name, url }], activeServer, ui: {...}, lastSeq: { "<server>/<session>": n } }`.
Токены — в системном keyring на десктопе (`keyring` crate, сервис
`aiddiction`, ключ `server:<id>`), на Android — `secrets.json` в
`internal_data_path` (приватная директория приложения; при желании позже —
Android Keystore через JNI).

## 7. Android

Повторяем схему money-manager-ng один в один, меняя имена.

1. `crates/aiddiction/Cargo.toml`:
   ```toml
   [lib]
   crate-type = ["lib", "cdylib"]

   [target.'cfg(target_os = "android")'.dependencies]
   gpui-mobile = { package = "gpui-pre-mobile", git = "https://github.com/longbridge/gpui-mobile", rev = "9075e3aa3eea812127f2c60ed66f0cd5798ff245" }  # тот же rev, что и в money-manager-ng, совместимый с gpui-kit 0.7
   android-activity = { version = "0.6", features = ["native-activity"] }
   android_logger = "0.15"
   log = "0.4"
   jni = "0.22"
   rustls-platform-verifier = "0.7"   # как в matterfast
   tracing = { workspace = true, features = ["log"] }
   ```
2. `lib.rs::android_main` — копия money-manager-ng: `STARTED`-флаг
   (повторный вызов → `exit(0)`), `android_logger` с тегом `aiddiction`,
   `jni::install_panic_hook()`, `XDG_CONFIG_HOME/XDG_DATA_HOME` →
   `app.internal_data_path()`, `jni::init_platform(&app)`,
   `Application::with_platform(platform.into_rc()).with_assets(..).run(start)`.
   Дополнительно: инициализация `rustls-platform-verifier` контекстом
   активности (посмотреть, как это сделано в matterfast `lib.rs`).
3. `android/` — скопировать Gradle-проект из `money-manager-ng/android`
   (Java-классы `dev/gpui/mobile` из примера gpui-mobile), заменить
   `namespace/applicationId` на `app.akergez.aiddiction`,
   `nativeLibraryName = "aiddiction"`, переменную версии
   `AIDDICTION_VERSION`. `minSdk = 26`, `abiFilters = arm64-v8a`. Манифест:
   только `INTERNET`.
4. `build-aux/build-android.sh` — адаптированная копия (cargo-ndk,
   профиль `dist`, подпись из `ANDROID_KEYSTORE_B64`).
5. Сборка вручную:
   ```sh
   export ANDROID_HOME=$HOME/Android/Sdk
   export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.0.12077973
   cargo ndk -t arm64-v8a --platform 26 -o android/app/src/main/jniLibs \
       build -p aiddiction --lib --release
   (cd android && ./gradlew assembleDebug)
   adb install -r android/app/build/outputs/apk/debug/app-debug.apk
   ```
   Только `--release`: debug-библиотека огромна, а отладочные метки рендерера
   роняют Vulkan эмулятора (опыт обоих референсов).
6. Специфика Android, которую нужно закрыть:
   - IME: многострочный ввод и composition в композере — проверить на
     реальном устройстве; в matterfast есть `build-aux/ime-tests.sh` —
     ориентир для регресс-проверок.
   - Обрывы сети и фон — раздел 6.5.
   - Нет зависимостей, которые тянут C-код с CMake (у matterfast это Opus):
     `rustls` вместо OpenSSL, `rusqlite` клиенту не нужен.
   - `keyring` не собирается под Android — держать под
     `cfg(not(target_os = "android"))`, абстракция `SecretStore`.
   - Сетевые вызовы не идут через Android Network Security Config, поэтому
     `http://` к серверу технически работает; в UI предупреждать о
     незашифрованном соединении.

## 8. Тестирование

| Уровень | Что | Как |
|---|---|---|
| Юнит, сервер | `routing.rs` (переименование id, владельцы, приостановка load, первый ответ на permission выигрывает), `journal.rs` (seq, вытеснение, truncated), канонизация путей, slug, парсер прогресса git | Таблицы вход/ожидаемые действия, без I/O |
| Интеграция, сервер | Полный цикл через `aiddiction-fake-agent`: создать проект → запустить инстанс → два WS-клиента → prompt → уведомления только владельцу → обрыв → attach с `afterSeq` → доигрывание; падение агента → ошибки ожидающим; fs-запросы агента вне проекта отклоняются | `#[tokio::test]`, сервер на случайном порту во временном `projects_root` |
| Fake agent | Сценарии: `echo`, `stream` (много чанков), `tool-calls` (с diff и terminal), `permission`, `big-frame` (сообщение > 128 КБ), `crash-after-N`, `slow` (для watchdog) | Реализован на стороне `Agent` крейта `agent-client-protocol`; сценарий из флага `--scenario` |
| Клиент, логика | `Thread::apply(SessionUpdate)` — склейка чанков, обновление tool call, plan; идемпотентность по `seq` | Обычные тесты + `#[gpui_kit::test]` для сущностей |
| Клиент, UI | Скриптовые сценарии `crates/aiddiction/tests/ui/*.script` (`wait/click/key/type/shot/expect/quit`), `build-aux/ui-tests.sh --headless` в контейнере с sway — как в money-manager-ng | `AIDDICTION_DEMO=1` поднимает сервер с fake-агентом в процессе; `AIDDICTION_SIZE=400x800` для телефонного layout |
| E2E с реальным агентом | Ручной чек-лист: Claude Code ACP, Gemini CLI — создание проекта из git, правка файла агентом с разрешением, отмена хода | Перед релизом |
| Android | Сборка APK в CI; ручная проверка на устройстве: ввод, поворот, фон/возврат с переподключением | `build-aux/build-android.sh` |

Правило из референсов: никогда не запускать клиент «попробовать» против
реального сервера с реальными проектами — только `AIDDICTION_DEMO=1` или
локальный сервер с временными каталогами
(`XDG_DATA_HOME=/tmp/aid/data XDG_CONFIG_HOME=/tmp/aid/config`).

## 9. Этапы

Каждый этап заканчивается рабочим состоянием и проходящими тестами.

### Этап 1. Скелет workspace (0.5–1 день)
- Workspace, toolchain, профили, пустые крейты, `CLAUDE.md` с командами.
- `aiddiction-fake-agent` со сценарием `echo`.
- **Готово, когда:** `cargo test --workspace` зелёный; `cargo run -p aiddiction`
  открывает пустое окно gpui-kit.

### Этап 2. Сервер: проекты и каталог (2–3 дня)
- Конфиг, токен, SQLite + миграции, REST проектов (пустые), `/agents`,
  `/health`, `/server`, `/events` со `snapshot`.
- Git-клонирование с прогрессом и askpass.
- `docs/API.md`.
- **Готово, когда:** через `curl`/`websocat` создаётся пустой и git-проект,
  прогресс приходит в `/events`, интеграционные тесты проходят.

### Этап 3. Сервер: агенты и ACP-прокси (4–6 дней)
- `AgentSupervisor`, запуск/остановка группы процессов, stderr tail,
  серверный `initialize`.
- `AcpProxy`: одно соединение, переписывание `cwd`, кеш `initialize`,
  переименование id, fs/* на сервере.
- Затем: несколько соединений, владельцы сессий, журнал + `attach`,
  watchdog, ping, `terminal/*`.
- **Готово, когда:** интеграционные сценарии из раздела 8 зелёные с
  fake-агентом; ручной прогон с реальным Claude Code ACP через
  `websocat` (initialize → session/new → prompt) даёт ответ.

### Этап 4. Клиент: серверы и проекты (3–4 дня)
- `runtime.rs`, `Settings`, `SecretStore`, `ServerConnection`.
- Экраны «Серверы», «Проекты», «Новый проект», «Проект»; адаптивный layout.
- Скриптовый режим + первые UI-сценарии.
- **Готово, когда:** с десктопа добавляется сервер, создаётся git-проект с
  прогресс-баром, список обновляется по событиям без перезагрузки.

### Этап 5. Клиент: ACP-чат (5–8 дней)
- ACP-клиент поверх `AcpSocket` по схеме Zed, `AgentSession`, `Thread`.
- Чат: сообщения, markdown, размышления, tool calls с diff/terminal,
  план, права, отмена, slash-команды, режимы/модели.
- Переподключение с `attach`/`afterSeq`.
- **Готово, когда:** с реальным агентом выполняется задача «создай файл и
  запусти тесты» с подтверждением прав; убийство сокета посреди хода
  (`ss -K` / перезапуск сети) не теряет сообщений.

### Этап 6. Android (2–4 дня)
- `android_main`, Gradle-проект, скрипт сборки, `SecretStore` для Android,
  rustls-platform-verifier.
- Отладка IME, жестов прокрутки, фона/возврата.
- **Готово, когда:** APK ставится на телефон, проходит полный сценарий
  этапов 4–5 против сервера в LAN/Tailscale.

### Этап 7. Полировка и релиз (2–3 дня)
- Список сессий проекта и возврат в старую сессию, индикаторы активности,
  уведомления о завершении хода (desktop notification; на Android — позже).
- CI: тесты, UI-тесты headless, сборка Linux tarball и APK по тегу,
  `check-version.sh`.
- README: установка сервера (systemd user unit), безопасность, сборка.

Итого ориентировочно 3–5 недель одного разработчика.

## 10. Риски и открытые вопросы

| Риск | Чем грозит | Что делаем |
|---|---|---|
| Агенты по-разному поддерживают `session/load`, `session/list`, повторный `initialize` | Невозможность вернуться к сессии после рестарта инстанса | Серверный `initialize` один раз + журнал; `session/load` только при `loadSession`; таблица `sessions` как запасной список |
| Большие JSON-строки (каталоги моделей > 100 КБ) | Обрезанные сообщения, зависшие запросы | Построчное чтение без лимита 64 КиБ, WS `max_message_size` ≥ 16 МБ, сценарий `big-frame` в fake-агенте |
| API `agent-client-protocol` 1.x меняется, unstable-фичи | Поломка клиента при обновлении | Пин `=1.1.0` (как у Zed), обновлять вместе с ориентиром на Zed; прокси сервера не зависит от типов крейта |
| Агент требует интерактивного логина (терминальная авторизация) | Агент «не работает» из клиента | MVP: сообщение «выполните логин на сервере: <команда>»; позже — терминальная авторизация через `terminal/*` сервера с выводом в клиент |
| Совместимость `gpui-mobile` rev с `gpui-kit` | Android-сборка не собирается | Брать тот же rev, что money-manager-ng на момент старта, обновлять синхронно с ним |
| Агент с правами пользователя сервера | Агент может испортить что угодно вне проекта | Документировать; отдельный пользователь/контейнер; позже — опциональный запуск агента через `bwrap`/podman |
| Утечка токена git | Компрометация репозиториев | askpass + env, не argv/конфиг; не логировать URL с userinfo |
| Несколько клиентов отвечают на один permission-запрос | Двойной ответ агенту | «Первый выигрывает» + `agent_request_resolved` остальным |

Открытые вопросы к владельцу проекта:

1. Нужна ли многопользовательность сервера в обозримом будущем (тогда токены
   на пользователя и `owner` у проектов закладываем сразу)?
2. Один инстанс агента на проект или разрешать несколько параллельных (разные
   агенты — да, тот же агент дважды — ?).
3. Нужен ли серверный список MCP-серверов, подставляемый в `session/new`?
4. Как сервер будет доступен с телефона: LAN, Tailscale, публичный адрес за
   reverse-proxy? От этого зависит, делать ли встроенный TLS в MVP.

## 11. Состояние прототипа (2026-10-24)

Собран первый прототип по разделам 4–7. Что сделано и чем он отличается от
плана выше.

### Что работает

- **Сервер** (`crates/aiddiction-server`): конфиг + переменные окружения
  `AIDDICTION_*`, токен (генерируется и хранится в data dir), проекты (пустые и
  `git clone` с прогрессом и `GIT_ASKPASS` для токена), каталог агентов
  (встроенные Claude/Codex, `[[agents]]` из конфига, custom через API),
  запуск агента через `bash -c` в группе процессов, остановка группы по
  SIGTERM→SIGKILL, хвост stderr, события `/events`, graceful shutdown по
  SIGINT/SIGTERM с остановкой агентов.
- **ACP-прокси** (`proxy.rs`): серверный `initialize` с кешем (клиентский
  `initialize` ждёт, если агент ещё стартует), переписывание `cwd` в
  `session/new|load|resume`, переименование id запросов per-socket, владельцы
  сессий, журнал с `seq` и `_aiddiction/attach` (ответ раньше доигрывания),
  эхо промпта в журнал (`user_message_chunk`), `_aiddiction/prompt_finished`,
  «первый ответ на permission выигрывает» + `_aiddiction/request_resolved`,
  `fs/read_text_file` и `fs/write_text_file` на сервере в пределах проекта.
- **Клиент** (`crates/aiddiction`): серверы (добавление с проверкой, выбор,
  удаление), проекты (список, создание пустого/из git, удаление), каталог
  агентов с сервера и форма custom-агента, запущенные агенты и их сессии,
  чат: markdown, размышления, tool calls с diff, план, запросы прав, стоп,
  режимы агента, подсказка slash-команд, переподключение с `attach`/`afterSeq`
  и `session/load` как запасной путь. Раскладка телефона (< 44 rem) — стек
  страниц с кнопкой «назад».
- **Проверено с настоящими агентами**: `claude-agent-acp` 0.95 и
  `codex-acp` 2.2.2 через прокси — `initialize` и `session/new` проходят,
  приходят режимы, модели и slash-команды.

### Добавлено после первого прототипа

- **Темы** — движок money-manager-ng и matterfast, библиотеки
  `gpui-adaptive-colors` (Material You-схема из цвета системы или выбранного)
  и `gpui-zed-themes` (темы Zed: установка из реестра их же `Browser`,
  хранение в `<data>/app.akergez.Aiddiction/themes`, ношение через `wear`).
  В приложении только тонкий слой `appearance.rs` (выбор в настройках) и
  страница Settings: режим System/Light/Dark, цвет, тема для светлого и
  тёмного режима, обзор реестра.
- **Стандартные места** — общий крейт `aiddiction-paths`: XDG-переменная, если
  задана, иначе стандартный каталог платформы (`dirs`: `~/.config` и
  `~/.local/share` на Linux, Application Support на macOS, AppData на
  Windows). Сервер: `<config>/aiddiction-server/config.toml`,
  `<data>/aiddiction-server/`; клиент: `<config>/app.akergez.Aiddiction/settings.json`
  (серверы и внешний вид).
- **Существующая папка как проект** — `ProjectSource::Existing { path }`:
  папка берётся как есть (`~` раскрывается, путь канонизируется, одна папка —
  один проект), у проекта заполняется `path`; при удалении проекта такая папка
  **никогда** не удаляется. В форме нового проекта — вариант «Existing folder»
  с обзором папок сервера (`GET /fs/dirs`).

### Ревью diff (как revdiff)

Встроенное ревью изменений проекта по образцу
[revdiff](https://github.com/umputun/revdiff): файлы слева деревом папок
(цепочки из одной папки свёрнуты, как в revdiff), diff справа с подсветкой
синтаксиса tree-sitter (обе версии файла собираются из diff и разбираются
целиком, так что многострочные комментарии и строки подсвечены верно) (по
умолчанию весь файл, переключается на компактный), аннотации на строку, на
ханк (слово «hunk» в тексте) и на файл, клавиши revdiff (`j/k`, `n/p`,
`[`/`]`, `a`/`Enter`, `A`, `d`, `Space`, `R`, `Ctrl+Enter`). Источники:
uncommitted (с untracked), staged, последний коммит, ветка против
main/master, любой ref. Вывод — формат revdiff байт в байт; «Send to agent»
отправляет его промптом в чат проекта с инструкцией (вопросы с `??` —
ответить, остальное — исправить) и очищает ревью для следующего круга.

Код в двух крейтах, не зависящих от Aiddiction, чтобы локальный standalone-
инструмент собрался из них без изменений: `diffreview` (модель, формат, git,
без UI — его использует и сервер) и `diffreview-ui` (вью на gpui-kit с
трейтом `ReviewBackend`; `LocalBackend` — git на этой машине). Aiddiction
добавляет только `ServerBackend` (REST) и страницу. Diff по сети идёт
обычным текстом `git diff`, разбирается на клиенте тем же парсером.

Не сделано: word diff, blame, поиск, история ревью, перенос длинных строк.
Подсветки нет для SQL: его грамматика в gpui-kit требует старый `cc`,
несовместимый с Android-крейтами.

### Отличия от плана (сознательные упрощения прототипа)

| План | Прототип | Почему / что дальше |
|---|---|---|
| SQLite | JSON-файлы (`projects.json`, `agents.json`) с атомарной записью | Хватает для одного пользователя; SQLite — когда появится история сессий |
| `agent-client-protocol` 1.1 в клиенте | Свой JSON-RPC поверх WS, обновления разбираются как `serde_json::Value` (`thread.rs`) | Неизвестный вид обновления не роняет сообщение; типы схемы можно подключить позже |
| `terminal/*` на сервере | Не объявляется; запросы отклоняются | Claude/Codex выполняют команды сами; нужно для агентов, которые полагаются на терминал клиента |
| События с данными (`snapshot`, `git_job.progress`) | События «что изменилось», клиент перечитывает список | Проще и устойчивее к пропускам; прогресс git — в `statusMessage` проекта |
| Watchdog тишины, лимит очереди сокета | Нет | Следующий шаг для долгих ходов и мёртвых клиентов |
| Keyring для токенов | `settings.json` с правами 0600 | Keyring на десктопе — следующий шаг |
| Терминальная авторизация агента | Нет | Агент нужно залогинить на сервере заранее (у claude/codex логин общий с CLI) |

### Тесты

- `cargo test --workspace` — юнит-тесты прокси (маршрутизация, журнал,
  attach, permissions, fs вне проекта), каталога, модели треда, скрипта и
  сквозной тест `crates/aiddiction-server/tests/end_to_end.rs` (сервер +
  fake-агент + `aiddiction-client`).
- `build-aux/ui-tests.sh --headless` в контейнере `localhost/aiddiction-ui` —
  сценарии `crates/aiddiction/tests/ui/*.script` (`first-chat`, `phone`,
  `custom-agent`, `existing-folder`, `settings`, `review`) с `AIDDICTION_DEMO=1`.

### Android

Обвязка готова: `cdylib`, `android_main` (как в money-manager-ng, тот же rev
gpui-mobile), Gradle-проект `android/` (`app.akergez.aiddiction`),
`build-aux/build-android.sh`, `build-aux/android-clang.sh` (хостовый clang +
sysroot и libunwind из NDK + rust-lld — для aarch64-хоста, где у NDK нет
своего clang).

**Библиотека собирается на этой машине**: `libaiddiction.so` (release, 47 МБ),
ELF aarch64 для Android 26, есть `android_main` и `ANativeActivity_onCreate`,
зависимости только системные (`liblog`, `libandroid`, `libdl`, `libm`, `libc`).
NDK r27c лежит в `~/Android/ndk/android-ndk-r27c`.

**APK локально не собирается — и это тупик**: `aapt2` (и остальные
build-tools) существуют только под x86_64, а x86-эмуляция через muvm/FEX
здесь не заводится; SDK в `~/tresse/.android-sdk` — пустые заглушки. APK
собирать на x86_64 (CI), как у matterfast и money-manager-ng:
`build-aux/build-android.sh` там работает с обычным `cargo ndk`.

### Следующие шаги

1. CI на x86_64 для APK (`build-aux/build-android.sh`) и проверка на телефоне: IME, фон и возврат, переподключение.
2. Watchdog и лимит очереди отправки в прокси; `terminal/*` на сервере.
3. Выбор модели (`session/set_model` / config options) в чате, отображение `usage_update`.
4. Keyring для токенов на десктопе, TLS (rustls) на сервере.
5. Список сессий после перезапуска агента (`session/list`) и возврат в них через `session/load`.

