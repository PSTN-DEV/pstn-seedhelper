# Seed Helper

Легкое приложение на Windows от [PSTN Squad](https://pstnsquad.ru) которое облегчает seeding серверов PSTN в Squad.

## Установка

Скачайте установщик последней версии здесь [Releases](https://github.com/PSTN-DEV/pstn-seedhelper/releases).

Требуется Windows 10 1809 или позднее — x64 и x86 сборки доступны.

## Фичи:

- Автоматическое присоединение к серверам PSTN.
- Авто-обновление
- Минимальная трата ресурсов компьютера

## Build

```bash
cargo build --release --target x86_64-pc-windows-msvc
```

## Known Issues (Известные Проблемы):
- Курсор мыши лагает в Eco-режиме. Связано с плохой имплементацией "software cursor" от разработчиков.
