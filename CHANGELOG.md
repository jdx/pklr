# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2](https://github.com/jdx/pklr/compare/v0.2.1...v0.2.2) - 2026-03-24

### Added

- add HTTP URL rewrite support ([#46](https://github.com/jdx/pklr/pull/46))

### Fixed

- strip inherited class definitions from extends and remote amends ([#48](https://github.com/jdx/pklr/pull/48))
- strip inherited class definitions from amends output ([#45](https://github.com/jdx/pklr/pull/45))

## [0.2.1](https://github.com/jdx/pklr/compare/v0.2.0...v0.2.1) - 2026-03-23

### Added

- expose set_http_client and eval_to_json_with_client ([#43](https://github.com/jdx/pklr/pull/43))

## [0.2.0](https://github.com/jdx/pklr/compare/v0.1.0...v0.2.0) - 2026-03-23

### Added

- support hk.pkl evaluation — output block, class functions, perf ([#41](https://github.com/jdx/pklr/pull/41))
- enforce open modifier on classes ([#38](https://github.com/jdx/pklr/pull/38))
- type constraints with runtime enforcement via is/as ([#39](https://github.com/jdx/pklr/pull/39))
- Set() now deduplicates elements ([#37](https://github.com/jdx/pklr/pull/37))
- implement is/as type operators ([#35](https://github.com/jdx/pklr/pull/35))
- type alias declarations (typealias) ([#33](https://github.com/jdx/pklr/pull/33))
- implement read() and read?() resource readers ([#36](https://github.com/jdx/pklr/pull/36))
- class inheritance (extends) and super keyword ([#30](https://github.com/jdx/pklr/pull/30))
- support extends for modules and classes ([#29](https://github.com/jdx/pklr/pull/29))
- this keyword for self-referencing within objects ([#27](https://github.com/jdx/pklr/pull/27))
- fully implement annotations (@Deprecated, @ModuleInfo, etc.) ([#25](https://github.com/jdx/pklr/pull/25))
- late binding for object amendment ([#23](https://github.com/jdx/pklr/pull/23))
- add import* glob import support ([#21](https://github.com/jdx/pklr/pull/21))
- support default elements/values in objects and mappings ([#22](https://github.com/jdx/pklr/pull/22))
- enforce property modifiers (hidden, const, abstract) ([#20](https://github.com/jdx/pklr/pull/20))
- async evaluator with HTTP/HTTPS/package:// import support ([#17](https://github.com/jdx/pklr/pull/17))
- add unicode escapes, NaN/Infinity, durations, and data sizes ([#16](https://github.com/jdx/pklr/pull/16))
- integer division, exponentiation, non-null assertion, pipe operator ([#18](https://github.com/jdx/pklr/pull/18))
- class definitions, outer keyword — v1 complete ([#15](https://github.com/jdx/pklr/pull/15))
- v1 part 2 — object amendment, module/annotation skipping, higher-order methods ([#14](https://github.com/jdx/pklr/pull/14))
- v1 language features ([#13](https://github.com/jdx/pklr/pull/13))

### Fixed

- address merged PR feedback — type aliases, depth guard, dead code ([#40](https://github.com/jdx/pklr/pull/40))
- address PR #29 feedback — dotted extends, HTTP class injection ([#31](https://github.com/jdx/pklr/pull/31))
- hk.pkl compatibility — parser and eval improvements ([#32](https://github.com/jdx/pklr/pull/32))
- add import cache to break circular imports ([#28](https://github.com/jdx/pklr/pull/28))
- *(parser)* support dotted type names in new expressions ([#26](https://github.com/jdx/pklr/pull/26))
- *(parser)* skip generic type params in new expressions ([#19](https://github.com/jdx/pklr/pull/19))
- *(parser)* add ?? operator and fix dynamic key parsing ([#11](https://github.com/jdx/pklr/pull/11))
- *(test)* address PR #9 review feedback ([#10](https://github.com/jdx/pklr/pull/10))
- *(eval)* guard integer div/mod against zero, remove unreachable Value::Mapping ([#5](https://github.com/jdx/pklr/pull/5))

### Other

- remove feature list and roadmap from README ([#42](https://github.com/jdx/pklr/pull/42))
- set MSRV to 1.88 and add cargo msrv verify to CI ([#24](https://github.com/jdx/pklr/pull/24))
- add CLAUDE.md and document supported pkl subset in README ([#12](https://github.com/jdx/pklr/pull/12))
- remove status section from README
- add comprehensive pkl feature test suite ([#9](https://github.com/jdx/pklr/pull/9))
- *(deps)* add miette, remove unused serde ([#8](https://github.com/jdx/pklr/pull/8))
- add mise.toml, CI workflow, and communique config ([#6](https://github.com/jdx/pklr/pull/6))
