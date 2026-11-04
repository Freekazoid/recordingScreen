# Документация Screen Recorder 4.0

| Документ | Что внутри | Когда читать |
|---|---|---|
| [`../PROJECT_MEMORY.md`](../PROJECT_MEMORY.md) | Живой контекст: AS-IS, проблемы, решения, метрики, открытые вопросы, журнал | В начале любого треда |
| [`../roadmap.md`](../roadmap.md) | Этапы, задачи, статусы, gates, риски, RACI, DoD, git-стратегия | Планирование и контроль |
| [`01-discovery.md`](01-discovery.md) | Discovery: пользователи, AS-IS, причины проблем, гипотезы, baseline | Перед проектированием |
| [`02-architecture-v2.md`](02-architecture-v2.md) | Целевая архитектура 4.0: Rust + native UI (egui) + Python sidecar, потоки, OS-матрица, права | Проектирование и реализация |
| [`03-adr.md`](03-adr.md) | Architecture Decision Records (ADR-001…015) | При изменении решений |
| [`04-test-plan.md`](04-test-plan.md) | Тестовая стратегия, регрессия, корпус, CI-гейты | Реализация и релиз |

## Как поддерживать

1. Любое значимое решение → `PROJECT_MEMORY.md` (раздел «Журнал») + ADR.
2. Любая задача/этап → `roadmap.md`, статус `[ ] → [~] → [x]`.
3. Изменение scope → пересмотр Gate в `roadmap.md`.
4. После этапа — обновить метрики и «Ближайшие 2 недели».

## Текущий статус

- Discovery: черновик готов, нужны интервью и baseline.
- Стек: Rust + нативный UI (egui/eframe) + Python 3.12 sidecar
  (Go + Wails — альтернатива).
- Git: ветка `rewrite/v4`, legacy-тег 3.2.194, версия `4.0.0-alpha.1`.
- Следующий Gate: G0 — подтвердить egui, scope и baseline.
