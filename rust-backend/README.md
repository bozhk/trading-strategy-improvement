# PulseBook — Rust Order-Flow Engine

Полный порт Python-движка (`backend/`) на Rust с идентичной торговой логикой:
absorption → breakout → retest, confluence score, структурные стопы в R-множителях,
риск-на-сделку сайзинг, circuit breakers, детерминированный replay без look-ahead
и жёстко заблокированный live-режим до прохождения статистического gate.

## Почему Rust рациональнее Python в order-flow трейдинге

| Критерий | Python | Rust |
|---|---|---|
| Обработка тика стакана | ~микросекунды-миллисекунды, GC-паузы и GIL | наносекунды-микросекунды, без GC и GIL |
| Латентность p99 | непредсказуемая (GC, GIL contention) | стабильная и предсказуемая |
| Параллельные WebSocket-стримы | asyncio на одном ядре из-за GIL | tokio использует все ядра |
| Ошибки типов/None | падают в рантайме, иногда на живых деньгах | ловятся компилятором до запуска |
| Гонки данных | возможны и трудноуловимы | исключены системой владения (borrow checker) |
| Память на инстанс | сотни МБ (интерпретатор + объекты) | десятки МБ (один статический бинарник) |
| Деплой | питон + venv + зависимости на сервере | один бинарник, ничего не нужно |

Что это даёт конкретно этой стратегии:

1. **Скорость реакции = качество исполнения.** Стратегия ловит поглощение стен
   и ретест — событие живёт секунды. Пока Python парсит JSON и ждёт GIL,
   цена уходит, и вы получаете худший fill. В Rust полный цикл
   «тик → решение» занимает микросекунды.
2. **Отсутствие GC-пауз.** Stop-the-world пауза сборщика мусора в момент
   каскада ликвидаций — это пропущенный стоп. В Rust пауз нет в принципе.
3. **Надёжность.** `Option<T>` вместо `None`-сюрпризов, `Result<T, E>` вместо
   необработанных исключений: движок не может «тихо упасть» посреди сессии.
4. **Честный replay.** Один и тот же типизированный код молотит миллионы
   исторических тиков в сотни раз быстрее — walk-forward валидация из часов
   превращается в минуты.
5. **Индустриальный стандарт.** HFT/маркет-мейкеры пишут исполнение на
   C++/Rust, а Python оставляют для исследований и отчётов — ровно по этим причинам.

Где Python остаётся лучше: исследования, pandas/notebooks, быстрые прототипы
гипотез. Рекомендуемая схема: **исследования в Python, исполнение в Rust**.

## Установка Rust

### Сервер (Linux — Ubuntu/Debian/Amazon Linux)

```bash
# 1. Компилятор C (нужен линковщику Rust)
sudo apt-get update && sudo apt-get install -y build-essential curl   # Ubuntu/Debian
# или: sudo dnf install -y gcc curl                                   # Fedora/Amazon Linux

# 2. rustup — официальный установщик тулчейна
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# 3. Активировать в текущей сессии (в новых сессиях подхватится автоматически)
source "$HOME/.cargo/env"

# 4. Проверить
rustc --version && cargo --version
```

### Клиент (локальная машина)

- **macOS:** `xcode-select --install`, затем та же команда rustup, что выше.
- **Windows:** скачайте `rustup-init.exe` с https://rustup.rs, установщик сам
  предложит поставить Visual Studio Build Tools (нужны для линковки).
  Либо через winget: `winget install Rustlang.Rustup`.
- **Linux:** как на сервере.

Клиенту (браузеру) сам Rust не нужен — dashboard это HTML/JS, который
раздаёт Rust-сервер. Rust на «клиенте» нужен только чтобы собирать
и запускать проект локально.

## Запуск проекта

```bash
cd rust-backend

# Режим разработки (быстрая сборка, медленный код)
cargo run

# Продакшен-сборка (медленная сборка, быстрый код) — всегда для реальной работы
cargo build --release
./target/release/pulsebook-engine

# Тесты
cargo test
```

Откройте `http://localhost:8000` — dashboard тот же, что у Python-версии
(шаблон и статика берутся из `../backend/templates` и `../backend/static`).

### Переменные окружения

```bash
PORT=8000                 # порт HTTP-сервера
DATA_MODE=demo            # demo (синтетика) | real (публичные стримы Bybit)
ACCOUNT_EQUITY=10000      # виртуальный капитал
RISK_PER_TRADE_PCT=0.005  # риск на сделку (0.5%)
TEMPLATES_DIR=../backend/templates
STATIC_DIR=../backend/static

# Пример: реальные данные Bybit на порту 9000
DATA_MODE=real PORT=9000 ./target/release/pulsebook-engine
```

`TRADING_MODE=live` намеренно вызывает мгновенный отказ запуска —
live-исполнение отсутствует в бинарнике и заблокировано до прохождения
статистического gate (200+ out-of-sample сделок, PF ≥ 1.2, положительное
expectancy, просадка ≤ 10%) и отдельной ревизии кода исполнения.

## Деплой на сервере как systemd-сервис

```bash
# Собрать на сервере (или скопировать готовый бинарник под ту же архитектуру)
cargo build --release
sudo cp target/release/pulsebook-engine /usr/local/bin/

sudo tee /etc/systemd/system/pulsebook.service > /dev/null <<'EOF'
[Unit]
Description=PulseBook order-flow engine
After=network-online.target

[Service]
ExecStart=/usr/local/bin/pulsebook-engine
WorkingDirectory=/opt/pulsebook
Environment=DATA_MODE=real
Environment=PORT=8000
Environment=TEMPLATES_DIR=/opt/pulsebook/templates
Environment=STATIC_DIR=/opt/pulsebook/static
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

sudo mkdir -p /opt/pulsebook
sudo cp -r ../backend/templates ../backend/static /opt/pulsebook/
sudo systemctl daemon-reload
sudo systemctl enable --now pulsebook
sudo systemctl status pulsebook
```

## Структура

```
rust-backend/
├── Cargo.toml          # зависимости (tokio, axum, socketioxide, ...)
└── src/
    ├── main.rs         # HTTP + Socket.IO сервер, broadcast-цикл
    ├── config.rs       # настройки из env, блокировка TRADING_MODE=live
    ├── models.rs       # OrderBook, Position, ClosedTrade, PendingSignal
    ├── brain.rs        # торговая логика: absorption → breakout → retest
    ├── execution.rs    # реалистичные fills: bid/ask, комиссии, slippage
    ├── readiness.rs    # статистический gate для live-режима
    ├── state.rs        # общее состояние + snapshot для dashboard
    ├── stream.rs       # Bybit WebSocket (real) и синтетика (demo)
    ├── scanner.rs      # отбор ликвидных перпетуалов по обороту
    ├── replay.rs       # детерминированный replay JSONL без look-ahead
    └── detail.rs       # payload деталей символа для модалки
```
