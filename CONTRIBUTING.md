# Участие в разработке

Спасибо, что хотите помочь RustCloud. Ошибки и предложения пишите в Issues, изменения
присылайте Pull Request'ом.

## Перед Pull Request

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd web && npm ci && npm run check && npm test
```

Новый код — с тестами. Изменился API — обновите документацию в `docs/`.

## Лицензия и соглашение участника (CLA)

RustCloud распространяется под [AGPL-3.0-or-later](LICENSE); `crates/plugin-api`,
`examples/plugin-template` и примеры кода в `docs/dev` — под MIT или Apache-2.0
(см. [NOTICE](NOTICE)).

Чтобы проект мог и дальше выпускать коммерческие версии и менять лицензию, участник
передаёт автору проекта право использовать свой вклад под любой лицензией — это
соглашение ниже. Авторские права на вклад остаются у вас, и вы можете использовать
его как угодно сами. Ваш вклад всегда будет доступен и под открытой лицензией проекта.

Чтобы принять соглашение, напишите в описании своего первого Pull Request:

> I have read the RustCloud Contributor License Agreement 1.0 and I agree to it.

Без этого Pull Request не будет принят. Мелочи, которые не охраняются авторским правом
(опечатка, одна строка), можно присылать и без соглашения.

### RustCloud Contributor License Agreement 1.0

The text below, in English, is the agreement; the Russian text above only explains it.

1. **Definitions.** "You" means the person or legal entity that submits a
   Contribution. "Contribution" means any work of authorship (code,
   documentation, images, translations and the like) that You intentionally
   submit to the RustCloud project, by a pull request, patch, issue or
   otherwise. "Maintainer" means AzimovIz, the author of RustCloud, and any
   person or entity to whom AzimovIz assigns the rights in the project.

2. **Copyright license.** You grant the Maintainer, and recipients of software
   distributed by the Maintainer, a perpetual, worldwide, non-exclusive,
   no-charge, royalty-free, irrevocable license to reproduce, prepare derivative
   works of, publicly display, publicly perform, sublicense and distribute Your
   Contributions and such derivative works, under any license terms, including
   proprietary ones.

3. **Patent license.** You grant the Maintainer, and recipients of software
   distributed by the Maintainer, a perpetual, worldwide, non-exclusive,
   no-charge, royalty-free, irrevocable patent license to make, have made, use,
   offer to sell, sell, import and otherwise transfer Your Contributions, where
   it covers patent claims licensable by You that are necessarily infringed by
   Your Contribution alone or by its combination with the project.

4. **Open licence.** The Maintainer will also make every Contribution that it
   accepts available under the licence of the part of the project it was
   submitted to (GNU AGPL-3.0-or-later, or MIT OR Apache-2.0).

5. **Your rights.** You keep the copyright in Your Contributions. Except for
   the licenses granted here, You reserve all rights in them.

6. **Your representations.** You represent that each Contribution is Your
   original work, or that You have the right to submit it under this agreement,
   and that, if Your employer has rights in it, Your employer has allowed You to
   submit it under this agreement. You will mark any part of a Contribution
   that is someone else's work and give its licence.

7. **No warranty.** You provide Your Contributions "as is", without warranties
   or conditions of any kind. The Maintainer is not obliged to accept or use any
   Contribution.
