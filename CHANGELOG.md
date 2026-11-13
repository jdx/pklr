# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [6.0.0](https://github.com/jdx/pklr/compare/v5.2.3...v6.0.0) - 2026-11-13

### Added

- *(stdlib)* add missing List, Set, Listing, Map and Bytes members ([#465](https://github.com/jdx/pklr/pull/465))
- *(eval)* add pkl:ref module ([#461](https://github.com/jdx/pklr/pull/461))
- *(eval)* reflect declared types with doc comments, annotations and locations ([#460](https://github.com/jdx/pklr/pull/460))
- *(eval)* check Listing and Mapping members when read ([#458](https://github.com/jdx/pklr/pull/458))
- *(eval)* [**breaking**] add pkl:reflect and Class members, bare getClass(), module-named type aliases ([#448](https://github.com/jdx/pklr/pull/448))
- *(eval)* give a module a class that getClass() answers ([#436](https://github.com/jdx/pklr/pull/436))
- *(eval)* enforce const scopes and const/fixed overrides ([#432](https://github.com/jdx/pklr/pull/432))
- *(eval)* GlobAccess for globs over a host-provided file system ([#423](https://github.com/jdx/pklr/pull/423))
- *(eval)* resource-specific capability hooks ([#421](https://github.com/jdx/pklr/pull/421))
- *(eval)* read modulepath: resources from the module path ([#420](https://github.com/jdx/pklr/pull/420))
- *(eval)* resolve modulepath: imports against a module path ([#415](https://github.com/jdx/pklr/pull/415))
- *(eval)* add allowed_modules policy like pkl --allowed-modules ([#412](https://github.com/jdx/pklr/pull/412))
- *(stdlib)* parse YAML with yaml.Parser and report JSON parse errors like pkl ([#405](https://github.com/jdx/pklr/pull/405))
- *(eval)* implement Any.getClass() and Class.simpleName ([#392](https://github.com/jdx/pklr/pull/392))

### Fixed

- *(render)* apply ConvertProperty annotations and render object keys as values ([#464](https://github.com/jdx/pklr/pull/464))
- *(eval)* match pkl on chained prefix operators, Boolean conditions, float printing and more ([#463](https://github.com/jdx/pklr/pull/463))
- *(eval)* resolve an inherited public member through pkl:ref when a local shadows it ([#467](https://github.com/jdx/pklr/pull/467))
- *(reflect)* mirror member types, hidden members and inherited union defaults ([#466](https://github.com/jdx/pklr/pull/466))
- *(eval)* Listing default, late-bound this, super.default and getOrDefault ([#457](https://github.com/jdx/pklr/pull/457))
- *(eval)* reject an amended module as the relativePathTo argument ([#456](https://github.com/jdx/pklr/pull/456))
- *(eval)* [**breaking**] typeAliasConstraint3 and localPropertyInAmendingModule ([#455](https://github.com/jdx/pklr/pull/455))
- *(eval)* IntSeq members, relativePathTo, toMapping amendment, spread error text ([#454](https://github.com/jdx/pklr/pull/454))
- *(eval)* raise the seven errors Pkl raises where pklr succeeded ([#453](https://github.com/jdx/pklr/pull/453))
- *(eval)* show an empty class instance by its class in type mismatch errors ([#452](https://github.com/jdx/pklr/pull/452))
- *(eval)* reject undeclared properties in amending modules, check plain overrides, add Bytes and List.toMap ([#449](https://github.com/jdx/pklr/pull/449))
- *(eval)* late-bind super calls and class bodies reading later members ([#451](https://github.com/jdx/pklr/pull/451))
- *(eval)* [**breaking**] type constraint failures (generic alias args, messages, Map every/any, Dynamic defaults) ([#450](https://github.com/jdx/pklr/pull/450))
- *(eval)* listing this/super reads, isDistinct, typed locals over existing listings, typed amend params ([#446](https://github.com/jdx/pklr/pull/446))
- *(eval)* module types, extends module, amending chains, glob keys ([#447](https://github.com/jdx/pklr/pull/447))
- *(eval)* Null(default), nested union defaults, converters over undefined members ([#443](https://github.com/jdx/pklr/pull/443))
- *(eval)* keep element constraints of a constrained generic type ([#442](https://github.com/jdx/pklr/pull/442))
- *(eval)* check the constraints of the standard library type aliases ([#435](https://github.com/jdx/pklr/pull/435))
- *(eval)* read List, Set and Listing members through ?. ([#445](https://github.com/jdx/pklr/pull/445))
- *(eval)* amend Mapping/Listing/Dynamic classes, replace mapping entries, pkl-style subscript and spread errors ([#439](https://github.com/jdx/pklr/pull/439))
- *(eval)* local members never override, list argument errors name the element ([#438](https://github.com/jdx/pklr/pull/438))
- *(eval)* resolve local members and type names in property constraints ([#437](https://github.com/jdx/pklr/pull/437))
- *(eval)* render List, Set, Map, Pair and IntSeq as Pcf calls ([#441](https://github.com/jdx/pklr/pull/441))
- *(eval)* stop a module that amends this from hanging a caught error ([#440](https://github.com/jdx/pklr/pull/440))
- *(eval)* reject a type qualified by a property, class or local ([#434](https://github.com/jdx/pklr/pull/434))
- *(eval)* accept a module and its amendments as the module type ([#430](https://github.com/jdx/pklr/pull/430))
- *(eval)* typed locals, late-bound super, and local overrides ([#429](https://github.com/jdx/pklr/pull/429))
- *(eval)* reject invalid glob imports and non-type names in declared types ([#431](https://github.com/jdx/pklr/pull/431))
- *(eval)* check new Listing/Mapping type arguments and reject object keys in JSON ([#427](https://github.com/jdx/pklr/pull/427))
- *(eval)* member predicates and element indexes on Dynamic amendments ([#426](https://github.com/jdx/pklr/pull/426))
- *(eval)* match pkl on small error cases ([#433](https://github.com/jdx/pklr/pull/433))
- *(eval)* report a violated type constraint by its source text ([#428](https://github.com/jdx/pklr/pull/428))
- *(eval)* match relative imports from module path modules as modulepath: URIs ([#424](https://github.com/jdx/pklr/pull/424))
- *(eval)* free a deep result and its evaluator without overflowing the caller's stack ([#425](https://github.com/jdx/pklr/pull/425))
- *(eval)* apply an amended function default in Mapping.getOrDefault ([#416](https://github.com/jdx/pklr/pull/416))
- *(eval)* match allowed_modules against the modulepath: uri ([#417](https://github.com/jdx/pklr/pull/417))
- *(test)* build unit tests under the eval-core feature set ([#418](https://github.com/jdx/pklr/pull/418))
- *(eval)* evaluate deeply nested modules instead of failing at depth 32 ([#414](https://github.com/jdx/pklr/pull/414))
- *(eval)* reject undefined properties and type aliases that replace a property ([#413](https://github.com/jdx/pklr/pull/413))
- *(eval)* support function default and getOrDefault on Mapping ([#403](https://github.com/jdx/pklr/pull/403))
- *(render)* reject pair, class, type alias and intseq values like pkl ([#404](https://github.com/jdx/pklr/pull/404))
- *(eval)* read amended members declared later in the body and bind local objects ([#406](https://github.com/jdx/pklr/pull/406))
- *(eval)* [**breaking**] reject unknown types in is/as and toDynamic on Dynamic/Mapping ([#396](https://github.com/jdx/pklr/pull/396))
- *(eval)* build imported listing-of-mappings members from the declared type ([#400](https://github.com/jdx/pklr/pull/400))
- *(eval)* make new { } elements of a typed Listing instances of its element class ([#393](https://github.com/jdx/pklr/pull/393))
- *(eval)* keep members of an untyped body that also has elements ([#399](https://github.com/jdx/pklr/pull/399))
- *(eval)* [**breaking**] tell an element-only Dynamic from a Listing, as pkl does ([#395](https://github.com/jdx/pklr/pull/395))
- *(eval)* keep constraint on a default union alternative ([#389](https://github.com/jdx/pklr/pull/389))
- *(eval)* reject amending a union property that has no default ([#394](https://github.com/jdx/pklr/pull/394))
- *(eval)* skip imported module classes when rendering the module ([#388](https://github.com/jdx/pklr/pull/388))
- *(eval)* amend an explicit null default as a Dynamic like pkl ([#391](https://github.com/jdx/pklr/pull/391))
- *(eval)* resolve mapping value types in the declaring module's scope ([#390](https://github.com/jdx/pklr/pull/390))
- *(eval)* [**breaking**] omit null properties in JSON output as pkl does by default ([#387](https://github.com/jdx/pklr/pull/387))

### Other

- *(deps)* lock file maintenance ([#377](https://github.com/jdx/pklr/pull/377))
- *(deps)* lock file maintenance ([#373](https://github.com/jdx/pklr/pull/373))
- *(deps)* lock file maintenance ([#367](https://github.com/jdx/pklr/pull/367))

## [5.2.3](https://github.com/jdx/pklr/compare/v5.2.2...v5.2.3) - 2026-11-02

### Fixed

- *(deps)* update rust crate fancy-regex to 0.20 ([#364](https://github.com/jdx/pklr/pull/364))

### Other

- *(deps)* lock file maintenance ([#366](https://github.com/jdx/pklr/pull/366))
- *(deps)* lock file maintenance ([#365](https://github.com/jdx/pklr/pull/365))
- *(deps)* lock file maintenance ([#361](https://github.com/jdx/pklr/pull/361))
- *(deps)* lock file maintenance ([#354](https://github.com/jdx/pklr/pull/354))

## [5.2.2](https://github.com/jdx/pklr/compare/v5.2.1...v5.2.2) - 2026-10-26

### Fixed

- *(eval)* apply an amended Mapping default to the entries the base declared ([#345](https://github.com/jdx/pklr/pull/345))
- *(eval)* evaluate an amended base lazily and in its template's scope ([#344](https://github.com/jdx/pklr/pull/344))

### Other

- *(deps)* lock file maintenance ([#353](https://github.com/jdx/pklr/pull/353))
- *(eval)* split object body evaluation into methods over shared state ([#350](https://github.com/jdx/pklr/pull/350))
- *(eval)* split module evaluation into one method per phase ([#349](https://github.com/jdx/pklr/pull/349))
- lint the evaluator without host IO and fix its dead-code warnings ([#348](https://github.com/jdx/pklr/pull/348))
- split eval.rs and parser.rs into submodules ([#347](https://github.com/jdx/pklr/pull/347))
- *(eval)* settle late-binding passes on data, not allocation ([#343](https://github.com/jdx/pklr/pull/343))

## [5.2.1](https://github.com/jdx/pklr/compare/v5.2.0...v5.2.1) - 2026-10-23

### Fixed

- *(eval)* rebuild an amended inherited object when a later module overrides what it reads ([#339](https://github.com/jdx/pklr/pull/339))
- *(eval)* infer class type for new { } entries in a typed Mapping ([#334](https://github.com/jdx/pklr/pull/334))
- *(eval)* allow assigning a class's union property that has no default ([#335](https://github.com/jdx/pklr/pull/335))

### Other

- *(eval)* stop the late-binding passes once the values settle ([#340](https://github.com/jdx/pklr/pull/340))

## [5.2.0](https://github.com/jdx/pklr/compare/v5.1.2...v5.2.0) - 2026-10-19

### Added

- *(http)* build the reqwest client lazily with http_client_with ([#320](https://github.com/jdx/pklr/pull/320))
- *(package)* support and verify ::sha256: checksums in package URIs ([#313](https://github.com/jdx/pklr/pull/313))

### Fixed

- *(eval)* keep module reads through an inner object that rebinds module ([#325](https://github.com/jdx/pklr/pull/325))
- *(eval)* ignore shadowed module receivers when narrowing imports ([#318](https://github.com/jdx/pklr/pull/318))
- *(eval)* retain imported constraint alias bindings ([#316](https://github.com/jdx/pklr/pull/316))

### Other

- *(deps)* lock file maintenance ([#327](https://github.com/jdx/pklr/pull/327))
- *(eval)* allocate shared names only when new (hk config 175.1M -> 173.4M instructions) ([#323](https://github.com/jdx/pklr/pull/323))
- *(eval)* resolve module identities once per amendment (hk config 176.4M -> 171.3M instructions) ([#322](https://github.com/jdx/pklr/pull/322))
- *(eval)* share property names as Arc<str> (hk config 194.3M -> 176.5M instructions) ([#321](https://github.com/jdx/pklr/pull/321))
- *(eval)* snapshot entry state only while locals are pending ([#319](https://github.com/jdx/pklr/pull/319))

## [5.1.2](https://github.com/jdx/pklr/compare/v5.1.1...v5.1.2) - 2026-10-16

### Other

- *(deps)* cargo update and bump zip to 9 ([#311](https://github.com/jdx/pklr/pull/311))
- *(deps)* lock file maintenance ([#305](https://github.com/jdx/pklr/pull/305))

## [5.1.1](https://github.com/jdx/pklr/compare/v5.1.0...v5.1.1) - 2026-10-07

### Fixed

- *(eval)* build the class for new through a re-exported typealias ([#300](https://github.com/jdx/pklr/pull/300))

## [5.1.0](https://github.com/jdx/pklr/compare/v5.0.1...v5.1.0) - 2026-10-07

### Added

- separate the reqwest HTTP backend feature ([#297](https://github.com/jdx/pklr/pull/297))

## [5.0.1](https://github.com/jdx/pklr/compare/v5.0.0...v5.0.1) - 2026-10-07

### Fixed

- *(eval)* resolve Windows file URIs for modules ([#295](https://github.com/jdx/pklr/pull/295))

## [5.0.0](https://github.com/jdx/pklr/compare/v4.0.0...v5.0.0) - 2026-10-07

### Added

- *(eval)* support function values: apply, applyToList, toString, amending, Mixin and pipe errors ([#267](https://github.com/jdx/pklr/pull/267))
- *(stdlib)* [**breaking**] implement Int, Float, Boolean, Duration, DataSize and pkl:math ([#274](https://github.com/jdx/pklr/pull/274))
- *(eval)* [**breaking**] print trace() messages in pkl's format ([#263](https://github.com/jdx/pklr/pull/263))
- *(stdlib)* [**breaking**] implement pkl:base String and Regex APIs ([#257](https://github.com/jdx/pklr/pull/257))
- *(eval)* PListRenderer, pkl:jsonnet and pkl:xml renderers ([#278](https://github.com/jdx/pklr/pull/278))
- *(eval)* [**breaking**] Pkl's JSON, YAML, properties and Pcf renderers and module output ([#264](https://github.com/jdx/pklr/pull/264))
- *(eval)* add pkl:test's catch and catchOrNull, and match pkl's messages for errors they test ([#258](https://github.com/jdx/pklr/pull/258))
- *(eval)* [**breaking**] evaluate members in dependency order and resolve names as Pkl does ([#251](https://github.com/jdx/pklr/pull/251))
- *(eval)* support IntSeq ([#234](https://github.com/jdx/pklr/pull/234))

### Fixed

- *(eval)* bind this for direct Mapping entries ([#294](https://github.com/jdx/pklr/pull/294))
- *(eval)* preserve inherited module property defaults ([#292](https://github.com/jdx/pklr/pull/292))
- *(deps)* update rust crates ([#288](https://github.com/jdx/pklr/pull/288))
- *(eval)* bound inherited output converter captures ([#290](https://github.com/jdx/pklr/pull/290))
- *(eval)* render typed property defaults and enforce fixed/const members ([#276](https://github.com/jdx/pklr/pull/276))
- *(eval)* resolve a constraint's unbound names on the checked value ([#277](https://github.com/jdx/pklr/pull/277))
- *(eval)* bind super in amending and extending modules ([#279](https://github.com/jdx/pklr/pull/279))
- *(eval)* evaluate an object's failed locals again as its properties bind ([#280](https://github.com/jdx/pklr/pull/280))
- *(eval)* keep the elements of Listing-valued mapping entry bodies ([#266](https://github.com/jdx/pklr/pull/266))
- *(parser)* [**breaking**] accept pkl 0.32 syntax and reject what pkl rejects ([#272](https://github.com/jdx/pklr/pull/272))
- *(eval)* infer `new { ... }` parent from a typed property annotation ([#270](https://github.com/jdx/pklr/pull/270))
- *(eval)* infer untyped new from declared and inherited property types ([#269](https://github.com/jdx/pklr/pull/269))
- *(lexer)* retain enclosing string token spans ([#286](https://github.com/jdx/pklr/pull/286))
- *(lexer)* [**breaking**] preserve trace source spans ([#285](https://github.com/jdx/pklr/pull/285))
- *(eval)* [**breaking**] read resources, read* globs and glob patterns like pkl ([#259](https://github.com/jdx/pklr/pull/259))
- JSON parser edge cases
- *(xml)* preserve rendered content and valid output ([#283](https://github.com/jdx/pklr/pull/283))
- *(eval)* amend inherited body listings once when amending an object ([#262](https://github.com/jdx/pklr/pull/262))
- *(eval)* amend receiver members from generator bodies ([#260](https://github.com/jdx/pklr/pull/260))
- *(eval)* resolve triple-dot module URIs like pkl ([#252](https://github.com/jdx/pklr/pull/252))
- *(eval)* check declared property types, including alias constraints ([#235](https://github.com/jdx/pklr/pull/235))
- *(eval)* [**breaking**] reject iterating non-collections and elements in typed objects ([#265](https://github.com/jdx/pklr/pull/265))
- *(parser)* [**breaking**] reject cyclic type aliases, misplaced default markers and invalid annotations ([#268](https://github.com/jdx/pklr/pull/268))
- *(eval)* [**breaking**] enforce class extension, instantiation and module version rules ([#261](https://github.com/jdx/pklr/pull/261))
- *(parser)* [**breaking**] reject duplicate members and invalid modifiers like pkl ([#254](https://github.com/jdx/pklr/pull/254))
- *(eval)* [**breaking**] implement Pkl equality for collections, objects, classes and functions ([#255](https://github.com/jdx/pklr/pull/255))
- *(eval)* [**breaking**] apply class-read rules when following requested classes ([#245](https://github.com/jdx/pklr/pull/245))
- *(eval)* merge body amendments into members produced by generators ([#253](https://github.com/jdx/pklr/pull/253))
- *(eval)* fix main build after #242 and #244 ([#246](https://github.com/jdx/pklr/pull/246))
- *(eval)* re-evaluate classes that read module properties by bare name ([#242](https://github.com/jdx/pklr/pull/242))
- *(eval)* keep types and values in separate namespaces in reference analysis ([#243](https://github.com/jdx/pklr/pull/243))
- *(eval)* follow module-level locals when narrowing imports ([#241](https://github.com/jdx/pklr/pull/241))
- *(eval)* don't assume built-in bindings for types redeclared in nested bodies ([#239](https://github.com/jdx/pklr/pull/239))
- *(eval)* resolve type aliases in lambda bodies ([#240](https://github.com/jdx/pklr/pull/240))
- *(eval)* resolve `module` in class bodies to the module's properties ([#236](https://github.com/jdx/pklr/pull/236))
- *(eval)* resolve local type aliases in constraint bases for narrowed imports ([#233](https://github.com/jdx/pklr/pull/233))

### Other

- *(deps)* lock file maintenance ([#284](https://github.com/jdx/pklr/pull/284))
- [**breaking**] synchronous capabilities, concurrent prefetch and an optional async feature ([#250](https://github.com/jdx/pklr/pull/250))
- measure instruction counts with tak and gate pull requests on them ([#282](https://github.com/jdx/pklr/pull/282))
- compare pklr with the pkl CLI on Apple's language snippet tests ([#249](https://github.com/jdx/pklr/pull/249))
- *(eval)* [**breaking**] cut scope copies, share AST nodes, memoize converters (hk config 11.6 ms -> 9.6 ms) ([#256](https://github.com/jdx/pklr/pull/256))
- *(value)* [**breaking**] share object keys, strings and lists in Value (hk config 14.6 ms -> 13.1 ms) ([#248](https://github.com/jdx/pklr/pull/248))
- *(eval)* [**breaking**] capture object scopes lazily and cut redundant work (hk config 17.2 ms -> 12.8 ms) ([#244](https://github.com/jdx/pklr/pull/244))
- *(eval)* make the evaluator core synchronous ([#238](https://github.com/jdx/pklr/pull/238))
- *(eval)* share lambda captures, skip unused outer, and more (hk config 26 ms -> 20 ms) ([#232](https://github.com/jdx/pklr/pull/232))

## [4.0.0](https://github.com/jdx/pklr/compare/v3.0.4...v4.0.0) - 2026-10-04

### Fixed

- *(eval)* resolve glob imports from a bare relative entry path ([#231](https://github.com/jdx/pklr/pull/231))

### Other

- *(eval)* evaluate only the glob-imported modules that are read (hk config 180 ms -> 24 ms) ([#229](https://github.com/jdx/pklr/pull/229))
- *(parser)* [**breaking**] share object bodies between the AST and evaluated objects ([#228](https://github.com/jdx/pklr/pull/228))
- *(eval)* update per-entry object scopes instead of rebuilding them ([#227](https://github.com/jdx/pklr/pull/227))
- *(eval)* [**breaking**] share lambdas and captured scopes (hk config 371 ms -> 190 ms) ([#225](https://github.com/jdx/pklr/pull/225))
- *(eval)* don't capture unused `this` aliases in nested objects ([#224](https://github.com/jdx/pklr/pull/224))
- *(eval)* cache imports and cut scope copying (2.6x faster on hk's config) ([#223](https://github.com/jdx/pklr/pull/223))
- *(eval)* stop copying the property map for every `this` refresh ([#221](https://github.com/jdx/pklr/pull/221))
- *(parser)* borrow source text instead of copying it per parser ([#220](https://github.com/jdx/pklr/pull/220))

## [3.0.4](https://github.com/jdx/pklr/compare/v3.0.3...v3.0.4) - 2026-10-03

### Fixed

- *(eval)* don't amend an enclosing object's same-named property ([#218](https://github.com/jdx/pklr/pull/218))

## [3.0.3](https://github.com/jdx/pklr/compare/v3.0.2...v3.0.3) - 2026-09-28

### Fixed

- *(eval)* preserve function parameter precedence in typed objects ([#212](https://github.com/jdx/pklr/pull/212))

### Other

- split evaluator, parser, lexer, and feature tests into modules ([#213](https://github.com/jdx/pklr/pull/213))

## [3.0.2](https://github.com/jdx/pklr/compare/v3.0.1...v3.0.2) - 2026-09-28

### Fixed

- resolve super in object amendments ([#210](https://github.com/jdx/pklr/pull/210))
- implement String.replaceLast for literal patterns ([#208](https://github.com/jdx/pklr/pull/208))
- evaluate when generators in mappings ([#207](https://github.com/jdx/pklr/pull/207))
- preserve class identity in inferred function results ([#209](https://github.com/jdx/pklr/pull/209))

## [3.0.1](https://github.com/jdx/pklr/compare/v3.0.0...v3.0.1) - 2026-09-27

### Fixed

- *(eval)* resolve names in enclosing bodies before an object's inherited members ([#200](https://github.com/jdx/pklr/pull/200))

## [3.0.0](https://github.com/jdx/pklr/compare/v2.0.5...v3.0.0) - 2026-09-22

### Added

- *(eval)* [**breaking**] support import() and import*() expressions ([#195](https://github.com/jdx/pklr/pull/195))

### Other

- [**breaking**] mark the lexer, AST, and error enums non_exhaustive ([#197](https://github.com/jdx/pklr/pull/197))

## [2.0.5](https://github.com/jdx/pklr/compare/v2.0.4...v2.0.5) - 2026-09-19

### Fixed

- *(eval)* amend inherited entries of untyped mappings instead of replacing them ([#189](https://github.com/jdx/pklr/pull/189))

## [2.0.4](https://github.com/jdx/pklr/compare/v2.0.3...v2.0.4) - 2026-09-14

### Fixed

- *(eval)* preserve lexical scope across object amendments ([#184](https://github.com/jdx/pklr/pull/184))

## [2.0.3](https://github.com/jdx/pklr/compare/v2.0.2...v2.0.3) - 2026-09-14

### Fixed

- *(eval)* preserve partial imports across amendments ([#182](https://github.com/jdx/pklr/pull/182))

## [2.0.2](https://github.com/jdx/pklr/compare/v2.0.1...v2.0.2) - 2026-09-14

### Fixed

- *(lexer)* strip CRLF from multiline strings ([#180](https://github.com/jdx/pklr/pull/180))
- *(eval)* preserve imports across nested mapping amendments ([#177](https://github.com/jdx/pklr/pull/177))

## [2.0.1](https://github.com/jdx/pklr/compare/v2.0.0...v2.0.1) - 2026-09-12

### Fixed

- *(eval)* keep typed mapping entries and hidden properties faithful to Pkl ([#171](https://github.com/jdx/pklr/pull/171))

## [2.0.0](https://github.com/jdx/pklr/compare/v1.5.1...v2.0.0) - 2026-08-26

### Added

- [**breaking**] make blocking evaluation the default ([#162](https://github.com/jdx/pklr/pull/162))

### Fixed

- *(eval)* support com.github.actions packages ([#164](https://github.com/jdx/pklr/pull/164))

## [1.5.1](https://github.com/jdx/pklr/compare/v1.5.0...v1.5.1) - 2026-08-26

### Fixed

- *(eval)* complete class-as-function semantics ([#161](https://github.com/jdx/pklr/pull/161))

## [1.5.0](https://github.com/jdx/pklr/compare/v1.4.0...v1.5.0) - 2026-08-23

### Added

- add preload_package to seed the package cache ([#157](https://github.com/jdx/pklr/pull/157))

## [1.4.0](https://github.com/jdx/pklr/compare/v1.3.0...v1.4.0) - 2026-08-12

### Added

- persist package downloads for offline evaluation ([#153](https://github.com/jdx/pklr/pull/153))

## [1.3.0](https://github.com/jdx/pklr/compare/v1.2.0...v1.3.0) - 2026-08-03

### Added

- track environment reads during evaluation ([#145](https://github.com/jdx/pklr/pull/145))

## [1.2.0](https://github.com/jdx/pklr/compare/v1.1.3...v1.2.0) - 2026-07-16

### Added

- support parser-only and capability-backed builds ([#131](https://github.com/jdx/pklr/pull/131))

### Fixed

- *(eval)* restore evaluator sync compatibility ([#135](https://github.com/jdx/pklr/pull/135))

## [1.1.3](https://github.com/jdx/pklr/compare/v1.1.2...v1.1.3) - 2026-07-06

### Fixed

- *(eval)* include sibling functions in partial imports ([#129](https://github.com/jdx/pklr/pull/129))

## [1.1.2](https://github.com/jdx/pklr/compare/v1.1.1...v1.1.2) - 2026-07-01

### Fixed

- *(eval)* treat object methods as whole import uses ([#126](https://github.com/jdx/pklr/pull/126))

## [1.1.1](https://github.com/jdx/pklr/compare/v1.1.0...v1.1.1) - 2026-07-01

### Fixed

- *(eval)* track import generic param fields ([#124](https://github.com/jdx/pklr/pull/124))
- *(eval)* resolve relative imports/amends against a remote base URL ([#112](https://github.com/jdx/pklr/pull/112))
- *(eval)* bind local lambdas in declaration order in object bodies ([#114](https://github.com/jdx/pklr/pull/114))
- *(eval)* preserve class identity (type_name) through amendment ([#115](https://github.com/jdx/pklr/pull/115))
- *(eval)* avoid evaluating unused import fields ([#123](https://github.com/jdx/pklr/pull/123))
- *(eval)* support filter() on Map/Mapping ([#111](https://github.com/jdx/pklr/pull/111))
- *(eval)* short-circuit && and || ([#113](https://github.com/jdx/pklr/pull/113))

### Other

- *(deps)* lock file maintenance lockfile maintenance ([#118](https://github.com/jdx/pklr/pull/118))

## [1.1.0](https://github.com/jdx/pklr/compare/v1.0.6...v1.1.0) - 2026-06-14

### Added

- support package://pkl.pkl-lang.org/pkl-pantry ([#108](https://github.com/jdx/pklr/pull/108))

## [1.0.6](https://github.com/jdx/pklr/compare/v1.0.5...v1.0.6) - 2026-06-11

### Fixed

- *(eval)* preserve mapping annotation value types ([#106](https://github.com/jdx/pklr/pull/106))

## [1.0.5](https://github.com/jdx/pklr/compare/v1.0.4...v1.0.5) - 2026-06-11

### Fixed

- *(eval)* harden import glob matching ([#105](https://github.com/jdx/pklr/pull/105))
- *(eval)* support import glob wildcards ([#103](https://github.com/jdx/pklr/pull/103))

## [1.0.4](https://github.com/jdx/pklr/compare/v1.0.3...v1.0.4) - 2026-06-09

### Fixed

- *(eval)* support rewritten package amends ([#101](https://github.com/jdx/pklr/pull/101))

## [1.0.3](https://github.com/jdx/pklr/compare/v1.0.2...v1.0.3) - 2026-06-09

### Fixed

- *(eval)* support string boolean conversion

## [1.0.2](https://github.com/jdx/pklr/compare/v1.0.1...v1.0.2) - 2026-06-09

### Fixed

- *(eval)* preserve explicit union mapping types ([#95](https://github.com/jdx/pklr/pull/95))

## [1.0.1](https://github.com/jdx/pklr/compare/v1.0.0...v1.0.1) - 2026-06-09

### Fixed

- preserve regex output compatibility ([#93](https://github.com/jdx/pklr/pull/93))

## [1.0.0](https://github.com/jdx/pklr/compare/v0.4.5...v1.0.0) - 2026-06-09

### Fixed

- *(eval)* skip unused imports ([#91](https://github.com/jdx/pklr/pull/91))

## [0.4.5](https://github.com/jdx/pklr/compare/v0.4.4...v0.4.5) - 2026-06-09

### Fixed

- *(eval)* preserve mapping locals ([#88](https://github.com/jdx/pklr/pull/88))

## [0.4.4](https://github.com/jdx/pklr/compare/v0.4.3...v0.4.4) - 2026-06-08

### Fixed

- *(eval)* preserve union mapping defaults ([#86](https://github.com/jdx/pklr/pull/86))

## [0.4.3](https://github.com/jdx/pklr/compare/v0.4.2...v0.4.3) - 2026-05-18

### Fixed

- *(deps)* update rust crate reqwest to 0.13 ([#66](https://github.com/jdx/pklr/pull/66))
- *(deps)* update rust crate zip to v8 ([#69](https://github.com/jdx/pklr/pull/69))

### Other

- *(deps)* lock file maintenance ([#76](https://github.com/jdx/pklr/pull/76))
- *(deps)* lock file maintenance ([#73](https://github.com/jdx/pklr/pull/73))
- *(deps)* lock file maintenance ([#72](https://github.com/jdx/pklr/pull/72))
- set dev profile debug to 1 ([#71](https://github.com/jdx/pklr/pull/71))

## [0.4.2](https://github.com/jdx/pklr/compare/v0.4.1...v0.4.2) - 2026-04-26

### Fixed

- *(eval)* make @Deprecated lazy — only warn on access ([#58](https://github.com/jdx/pklr/pull/58))

## [0.4.1](https://github.com/jdx/pklr/compare/v0.4.0...v0.4.1) - 2026-04-07

### Fixed

- resolve nullable outer property access and semicolons ([#54](https://github.com/jdx/pklr/pull/54))

## [0.4.0](https://github.com/jdx/pklr/compare/v0.3.0...v0.4.0) - 2026-03-25

### Fixed

- extract converters from amends/extends base modules ([#52](https://github.com/jdx/pklr/pull/52))

## [0.3.0](https://github.com/jdx/pklr/compare/v0.2.2...v0.3.0) - 2026-03-24

### Added

- implement output.renderer.converters support ([#49](https://github.com/jdx/pklr/pull/49))

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
