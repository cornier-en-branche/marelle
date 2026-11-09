# Marelle в Termux

[English](TERMUX.md) | Русский

Marelle работает в [Termux](https://termux.dev/en/), терминале для Android. На этой странице: как установить программу, как дать ей API-ключ и как решить самые вероятные проблемы.

> **Статус.** Android-бинарники собирает CI этого репозитория, и CI проверяет, что они компилируются, линкуются и имеют правильную архитектуру. На телефоне их пока никто не запускал, поэтому первые отчёты очень важны. Если что-то пойдёт не так, заведите issue и приложите вывод `marelle version` и `uname -m`.

## Что нужно

- Android 7.0 или новее. Бинарники собраны под API level 24.
- Termux, установленный из [F-Droid](https://github.com/termux/termux-app#f-droid) или с [GitHub](https://github.com/termux/termux-app#github).
- API-ключ провайдера, например Z.ai.

## Установка готового бинарника

Сначала узнайте, какая сборка подходит телефону:

```sh
uname -m
```

`aarch64` почти у всех телефонов последних лет. `armv7l` и `armv8l` означают 32-битную систему. Подходящие имена архивов: `aarch64-linux-android` и `armv7-linux-androideabi`. Для `x86_64` (эмулятор или Chromebook) собирайте на устройстве, как описано в следующем разделе.

Затем скачайте, проверьте и установите. Свежую версию берите на [странице релизов](https://github.com/cornier-en-branche/marelle/releases).

```sh
VERSION=v0.1.0-alpha.1
ARCH=aarch64-linux-android        # или armv7-linux-androideabi
BASE=https://github.com/cornier-en-branche/marelle/releases/download/$VERSION

cd ~
curl -fLO "$BASE/marelle-$VERSION-$ARCH.tar.gz"
curl -fLO "$BASE/marelle-$VERSION-$ARCH.tar.gz.sha256"
sha256sum -c "marelle-$VERSION-$ARCH.tar.gz.sha256"
tar xzf "marelle-$VERSION-$ARCH.tar.gz"
install -m 755 "marelle-$VERSION-$ARCH/marelle" "$PREFIX/bin/marelle"
marelle version
```

`sha256sum` должен ответить `OK`. Если нет, удалите файлы и скачайте заново.

## Или сборка на телефоне

Подходит, если релиза для вашего процессора нет или нужен более новый коммит.

```sh
pkg update && pkg upgrade
pkg install rust git
termux-wake-lock
cargo install --locked --root "$PREFIX" --git https://github.com/cornier-en-branche/marelle marelle-cli
marelle version
```

- Берите пакет Termux. `rustup` в Termux не работает, а `pkg install rust` заодно ставит нужный сборке компилятор C (`clang`).
- `termux-wake-lock` не даёт Android усыпить Termux, когда гаснет экран. Снять блокировку можно командой `termux-wake-unlock`.
- Сборка компилирует около сотни крейтов. Это небыстро, и телефон нагреется. Поставьте его на зарядку.
- Если сборке не хватает памяти, уменьшите число потоков и отключите оптимизацию на этапе компоновки:

  ```sh
  CARGO_BUILD_JOBS=1 CARGO_PROFILE_RELEASE_LTO=off \
    cargo install --locked --root "$PREFIX" --git https://github.com/cornier-en-branche/marelle marelle-cli
  ```

Если сборка обрывается с `[Process completed (signal 9)]`, смотрите раздел «Если что-то не работает» ниже.

## Ключ

Marelle не хранит ключ в настройках. Он берёт его из переменной окружения или из файла, который вы укажете. В Termux файл в домашней папке доступен только самому приложению.

```sh
mkdir -p ~/.config/marelle
( umask 077; read -rsp "Ключ Z.ai: " key; echo; printf '%s' "$key" > ~/.config/marelle/zai.key )
```

Ключ вводится без отображения на экране и не попадает в историю команд. Вставьте его и нажмите Enter.

Чтобы настройки сохранялись, добавьте в `~/.bashrc` строки без секретов:

```sh
cat >> ~/.bashrc <<'EOF'
export MARELLE_PROVIDERS__ZAI__API_KEY_FILE="$HOME/.config/marelle/zai.key"
export MARELLE_CHAT__PROVIDER=zai
export MARELLE_CHAT__MODEL=MODEL
EOF
source ~/.bashrc
```

Вместо `MODEL` подставьте название модели из списка вашего провайдера. `marelle providers` показывает всех известных провайдеров и то, найден ли ключ. Если у вас ключ GLM Coding Plan, берите провайдер `zai_coding` вместо `zai`, а файл ключа задавайте через `MARELLE_PROVIDERS__ZAI_CODING__API_KEY_FILE`.

Пока настройки читаются только из переменных окружения. Файлы настроек следующий шаг [плана](ARCHITECTURE.md#roadmap).

## Первый разговор

```sh
marelle providers                              # какие провайдеры есть и найден ли ключ
marelle chat "Объясни игру в классики одним предложением"
marelle chat                                   # диалог: /help покажет команды, /exit выйдет
git diff | marelle chat "Проверь этот diff:" - # "-" означает то, что пришло по конвейеру
```

Ответ уходит в stdout. Рассуждения (`--show-reasoning`), число токенов (`--show-usage`), заметки и ошибки уходят в stderr, поэтому `marelle chat "..." > answer.txt` сохранит только ответ.

Интерфейс говорит по-английски, по-русски, по-французски и по-украински: `MARELLE_LANG=ru marelle help`.

## Ограничения этой версии

- Настройки берутся только из окружения и флагов.
- В диалоге нет редактирования строки и истории, кроме того, что даёт сам терминал.
- Ctrl-C завершает программу целиком, а не только текущий ответ.
- Пока только чат: инструментов и цикла агента ещё нет.

## Если что-то не работает

**`marelle: command not found`.** Проверьте, что файл `$PREFIX/bin/marelle` есть. Если вы ставили через `cargo install` без `--root`, бинарник лежит в `~/.cargo/bin`.

**`Exec format error` или `cannot execute binary file`.** Это сборка для другого процессора. Сравните `uname -m` с именем архива.

**`Cannot talk to the provider`.** Проверьте, что телефон в сети и что не мешает VPN или прокси. У Marelle свой список доверенных удостоверяющих центров, системный список Android он не использует. За прокси, который перехватывает HTTPS, укажите сертификат прокси через `export SSL_CERT_FILE=/path/to/ca.pem`, а сам прокси через `export MARELLE_NETWORK__PROXY=http://host:port`.

**Сборка обрывается с `signal 9`.** Android завершает долгие фоновые процессы (phantom process killer). На Android 14 и новее включите режим разработчика и выберите «Disable child process restrictions». Для Android 12 и 13 нужна команда ADB, её описывает это [руководство](https://cosyra.com/guides/termux-signal-9-fix.html). Готовый бинарник эту проблему обходит.

**Ключ отклонён (HTTP 401).** Проверьте ключ и аккаунт, к которому он относится. `marelle providers` показывает, где каждый провайдер ищет свой ключ.

## Обновление и удаление

Чтобы обновиться, повторите шаги установки с новой версией. Чтобы удалить Marelle, выполните `rm "$PREFIX/bin/marelle"`. Папку `~/.config/marelle` тоже удалите, если хотите убрать файлы с ключами.
