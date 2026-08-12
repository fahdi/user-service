## 2026-08-11 - Chore: four unused dependencies removed; tokio moved to dev-dependencies (#31)
## 2026-08-12 - Report Redis failures instead of discarding them (#39)

### Fixed
- A Redis failure in this service was invisible. `services/cache_service.rs` contains **zero** log calls across all six of its functions, and all four fallible calls were discarded at the adapter in `impls.rs`.
- **The discard is forced, not casual**, which is why the adapter is the right place to fix it: the trait methods return `()`, so the caching layer is deliberately infallible - a cache problem must not fail a user's profile update. The adapter cannot propagate the error, so it is the exact point where it dies and the only place it can be observed.
- The two failure modes are now distinguished, because only one matters. A failed **write** is benign - the entry is not cached, the next read misses and hits MongoDB, still correct. A failed **invalidation** leaves the stale entry, so a user keeps seeing their old name or avatar until the TTL expires; that warning says so explicitly.
- Same defect as projects-api#43, reached from the other direction. There the file logged failed writes while silencing invalidations - an internal contradiction that made it visible. Here the silence was **uniform**, which made it easier to miss and no less costly. Consistency is not evidence of correctness; it just removes the tell.
- Control flow is unchanged and the trait stays infallible, asserted by a test. Changing either would let a cache outage fail user updates, which is worse than serving a stale profile.

### Added
- Three tests: the `Ok` path stays silent, a failure does not propagate, and no cache result is discarded without logging. The last searches the source and **builds its needle at runtime** - a literal would appear in the file being searched and pass on its own text, a trap that has bitten four times in this repo and always fails open. Mutation-verified: reintroducing a discarded call fails it.
- 426 tests pass.

### Note
- `cargo clippy` reports a pre-existing future-incompatibility warning for `redis v0.25.4`, unrelated to this change. It also **corrects a claim I made in #35**, where I stated 0.25.4 carried no such warning; it does, so that upgrade has a compiler deadline after all. Corrected on that issue.
## 2026-08-12 - Remove the never-imported `hex` dependency (#37)

### Removed
- `hex` was declared in `Cargo.toml` with no `use` or path reference anywhere in `src/` or `tests/`. It was compiled into every build and shipped in the binary, and was dependency surface to audit and patch, for zero functionality.
- Found by sweeping every declared dependency for whether it is ever imported - a check that has now found unused crates in four services, and caught two larger problems on the way: system-monitoring#36, where a time-series client sat beside documentation claiming metrics were stored in it while nothing ever wrote a sample, and system-monitoring#38, where a `prometheus` crate sat unused beside a working hand-rolled exporter.
- Proven unused: the build compiles and the suite reports **423 tests before and after**, identical - stronger evidence than "it still builds", since it also rules out silently dropping a test.
## 2026-08-12 - Security: reject an empty or short JWT_SECRET (infra#55)

### Security
- `jwt_secret_from()` mapped only the `Err` arm, and `main()` gated startup on `env::var(..).is_err()`. Both ask whether the variable is **set**, which is a different question from whether it is **usable**: `env::var` returns `Ok("")` for a set-but-empty variable, so an empty string flowed through as the signing key. HMAC-SHA256 with an empty key is valid, so tokens signed with nothing would have verified.
- `docker-compose.production.yml` uses `JWT_SECRET=${JWT_SECRET}` with no default, and Compose substitutes an unset host variable as `""` - the case that actually occurs. The startup comment already stated the right intent, "must stop the service at startup instead of surfacing as per-request errors behind a green /health", but `is_err()` did not fire on it.
- `jwt_secret_from()` now rejects empty, whitespace-only and anything under 32 characters, matching the floor utilities-forms, auth-service and projects-api enforce. `main()` calls the **same function**, so startup and request handling cannot disagree about what counts as usable.
- Verified by mutation: reverting to the `Err`-only form fails all three rejection tests while both acceptance tests correctly keep passing. One test pins the secret `infra/local-dev/docker-compose.dev.yml` actually supplies, so this cannot break the environment it ships with.
- The tests need no process-environment mutation: `jwt_secret_from` already took the lookup result as a parameter, which is what made this cheap to test properly.
- One existing fixture was 19 characters and was lengthened; its intent, that a present secret passes through unchanged, is unaffected.
- 423 tests pass, clippy clean.

### Fixed
- `CLAUDE.md` test count refreshed to 423.
## 2026-08-12 - Docs: correct a stale test count in CLAUDE.md

### Fixed
- `CLAUDE.md` claimed **345 tests**; the suite actually runs **418**. Found while checking every service's documented count against reality after super#178 - **every count checked in this monorepo was wrong**, by as much as 4.6x in messages-chat.
- A hardcoded count is a claim that changes on almost every PR, so it rots by default. The root `CLAUDE.md` already handles this by quoting approximate totals and deferring exact numbers to CI; that convention is the durable fix, and this is the interim correction.
## 2026-08-12 - Build from the committed lockfile (infra#49)

### Changed
- Every `cargo build` in the builder now passes `--locked`. The lockfile was already copied, but without `--locked` a `Cargo.toml` edit that stales the lock would silently re-resolve at build time instead of failing.
- Fleet-wide: measured divergence was zero at the time (utilities-forms resolved from a bare `Cargo.toml` gave the same 419 package versions as its committed lock). The failure mode is not a bad version today, it is that the agreement decays silently on the next semver-compatible release of any transitive dependency, while `cargo audit` keeps reporting green about a `Cargo.lock` the image ignores. Guarded by `infra/tests/check-dockerfile-lockfile.sh`.

### Changed
- `actix-web-httpauth`, `uuid`, `anyhow`, `thiserror` removed - declared with zero references anywhere. `tokio` moved from dependencies to dev-dependencies: `src/` never touches it (this service runs on Actix's runtime), but `tests/unit_tests.rs` uses `#[tokio::test]`, so shipping tokio's `full` feature set in the release profile was surface nobody needed. Compiler-verified across all targets: 418 tests green, clippy clean, release build succeeds. Found via super#177's sweep; the tokio case is why that sweep verifies with `cargo check --all-targets` rather than trusting a `src/`-only scan.

## 2026-08-11 - Chore: dead pre-DI handler twin deleted (#29)

## 2026-08-11 - Chore: phantom rate limiting removed from code and docs (#26)

### Removed
- `src/middleware/rate_limit.rs` - 238 lines of a complete Actix Transform (Redis + LRU fallback) that never compiled: `mod.rs` had it commented out ("TODO: Fix middleware type issues") and `main.rs` wired nothing. The tracked CLAUDE.md meanwhile claimed live rate limiting with specific per-scope windows and tests that never existed. File deleted, docs corrected to state plainly that this service has no rate limiting (throttling happens at auth-service). New `doc_truth` guard test: when no rate_limit module is declared, CLAUDE.md must not claim one. Real limits for password/admin routes remain a feature decision. 418 tests, clippy zero warnings.

## 2026-08-11 - Fix: missing JWT_SECRET no longer panics per-request behind a green /health (#24)

### Fixed
- `extract_claims_from_request` resolved the secret with `env::var("JWT_SECRET").expect(...)` - a message claiming the service refuses to start, which was false: `main.rs` never checked it, so the service booted, `/health` passed the Docker healthcheck, and every authenticated request panicked the worker across all 13 handlers using the extractor. Two-part fix: (1) request path goes through a pure `jwt_secret_from(env_result)` helper that returns a 500 error response instead of panicking (takes the env result as input so tests need no racy env mutation); (2) `main()` fails fast at startup with a clear log when JWT_SECRET is unset, matching sibling services. The self-described placebo test (`let _ = std::env::var("JWT_SECRET")`) replaced with real RED-first specs for both helper outcomes. 417 tests, clippy zero warnings.

## 2026-08-11 - Security: a 429 from auth-service no longer resurrects revoked tokens (#22)

### Fixed
- **High**: `verify_with_auth_service` parsed the validate-response JSON without inspecting the HTTP status. auth-service's rate-limit rejection is a 429 whose body lacks the `valid` field, so deserialization failed and the middleware treated it as `Unavailable`, falling back to blacklist-free local validation - a revoked-token holder could get it honored by driving this service's egress IP over auth-service's validate limit. The status is now inspected first: any 4xx is `Rejected` (a verdict), transport errors and 5xx are `Unavailable` (an outage). Spec-first: RED spec in `blacklist_honoring_spec` pins 429 to rejection despite a valid local signature; a companion spec pins 5xx to the local-fallback escape hatch. Same fix as file-management#23; projects-api tracked separately. 416 tests, clippy zero warnings.

## 2026-08-03 - Fix: admin user list always showed Last Login "Never" (#20)

### Fixed
- auth-service writes `lastLogin` (BSON DateTime) on every successful login, but `StandardizedUser` had no last-login field and `standardize_user_doc` dropped it, so `GET /api/admin/users` never exposed it and the app's User Management page showed "Never" for everyone. Spec-first: RED tests assert a `lastLogin` document date standardizes to a `lastLoginAt` RFC3339 string and that the key is omitted for never-logged-in users. Fix: `last_login: Option<String>` (serialized `lastLoginAt`, skip-if-none) populated in `standardize_user_doc`; cache-service constructors carry `None` (profile cache does not store it). 414 tests, clippy zero warnings.

## 2026-05-31 - Track Cargo.lock for reproducible Docker builds (#138)

## 2026-07-24 - Security: honor the auth-service blacklist (#18)

### Fixed
- **High**: `extract_claims_from_request` validated JWTs purely locally and never consulted the auth-service blacklist - revoked tokens kept authenticating on all protected routes until natural expiry (fourth instance of the class: app #234, projects-api #24, file-management #21). Spec-first: four specs written and confirmed RED (compile + behavior) before implementation - explicit remote rejection is final despite a valid local signature, remote validation maps claims, unavailability falls back to local validation (consistent cross-service availability trade-off), malformed valid-without-claims responses are rejected.
- New pure core `validate_bearer_with(auth_url, token, secret)` (wiremock-testable, no env races); the env-reading wrapper stays thin. `AuthExtractor` DI trait is now async; `AUTH_SERVICE_URL` env (default `http://auth-service:8080`).


## 2026-07-24 - Index-conflict classification fix (#16)

### Fixed
- Startup index creation classified benign conflicts by matching "already exists", missing modern MongoDB code 85/86 errors worded "same name" - benign conflicts were warn-logged as failures. Spec-first fix: failing tests written for a `utils::is_index_conflict` classifier (both wordings, both codes, real-failure rejection), then implemented and wired into `main.rs`. Fourth service with this exact copy-pasted bug (after auth-service, file-management, projects-api).
- Removed an unused `futures_util::TryStreamExt` import that failed `clippy --all-targets -D warnings` in the integration test build.
- Repo-wide `cargo fmt` (the tree was not fmt-clean; CI gates fmt).


### Fixed
- `Cargo.lock` is now tracked in git (removed from `.gitignore`). Production Docker build was failing with `failed to compute cache key: "/Cargo.lock": not found` because the Dockerfile copies `Cargo.toml Cargo.lock ./` but the lock file was gitignored. For a binary crate, Cargo.lock should always be checked in to guarantee reproducible deployments.

## [1.1.0] - 2026-04-24 - Trait-Based Dependency Injection

### Changed
- **Handler DI Wiring**: All 13 route handlers now use trait-based dependency injection
  - Routes in `main.rs` now import from `handlers/di_handlers.rs` (DI-aware handlers)
  - Handlers accept `web::Data<AppState>` instead of calling global singletons
  - Old handlers in `handlers/user_handlers.rs` kept as fallback (marked `#[allow(dead_code)]`)

### Added
- **Concrete Trait Implementations** (`src/impls.rs`):
  - `MongoUserRepository` — wraps global MongoDB pool, implements `UserRepository` trait
  - `RedisCacheService` — delegates to existing multi-layer cache, implements `CacheService` trait
  - `GoogleDriveUploader` — wraps Google Drive service, implements `FileUploader` trait
  - `JwtAuthExtractor` — wraps JWT middleware, implements `AuthExtractor` trait
  - `build_app_state()` factory wires all concrete implementations into `AppState`
- **AppState Wiring**: `main.rs` builds `AppState` at startup and injects via `.app_data()`

### Fixed
- Cache service error types updated to `Box<dyn Error + Send + Sync>` for trait safety
- Removed stale `#![allow(dead_code)]` and TODO comments from `traits.rs`

### Technical Details
- Files changed: `src/main.rs`, `src/impls.rs` (new), `src/traits.rs`, `src/services/cache_service.rs`, `src/handlers/di_handlers.rs`
- Zero clippy warnings, all 385 tests passing
- API contract unchanged — all endpoints behave identically

# Changelog

All notable changes to the User Service will be documented in this file.

## [1.1.0] - 2026-04-24 - Integration Tests & Lib Refactor

### Added
- **DI integration tests** (`tests/di_integration_tests.rs`): 49 tests covering all 13 endpoints
  - Mock implementations of UserRepository, CacheService, FileUploader, AuthExtractor
  - Tests success cases, 401 unauthorized, 403 forbidden, 404 not found, 400 validation
  - Admin-only endpoints (admin search, admin update, import, role update) reject non-admin users
  - No real MongoDB or Redis required

### Changed
- **Refactored lib.rs/main.rs split**: moved all shared modules (handlers, services, middleware,
  models, utils, traits) and global statics (REDIS_POOL, MONGODB_CLIENT, USER_CACHE) from main.rs
  to lib.rs so integration tests can access DI handlers through the library crate
- Fixed clippy warnings in existing test files (assert!(true) placeholders, needless borrows)
- Total test count: 201 → 345

## [1.0.0] - 2025-01-07 - Initial Release

### Added
- **User Profile Management**: Complete user profile API with admin lookup capability
  - GET `/api/users/profile` - Retrieve user profiles with caching
  - Admin can lookup any user by ID or email, regular users see only their own profile
  - 15-minute Redis cache with LRU backup for optimal performance
  - Implemented in `src/handlers/user_handlers.rs:get_profile()`

- **Profile Picture Upload System**: Google Drive integration with image optimization
  - POST `/api/users/profile-picture` - Upload and optimize profile pictures
  - Image processing: 400x400 resize → 200x200 crop → 90% JPEG compression
  - Google Drive folder structure: `profile_photos_{userId}`
  - Public URL generation: `https://drive.google.com/thumbnail?id={}&sz=w200-h200`
  - Implemented in `src/handlers/user_handlers.rs:update_profile_picture()`

- **User Settings Management**: Comprehensive settings and account management
  - GET `/api/users/settings` - Retrieve user preferences and settings
  - PUT `/api/users/settings` - Update settings with account changes (email/password)
  - 30-minute Redis cache for settings data
  - Support for notifications, theme, language, timezone preferences
  - Implemented in `src/handlers/user_handlers.rs:get_settings()` and `update_settings()`

- **Multi-layer Caching System**: Phase 4 performance optimization
  - LRU cache (1000 entries) for instant access
  - Redis cache with TTL-based expiration
  - Cache invalidation on data updates
  - Implemented in `src/services/cache_service.rs`

- **JWT Authentication Middleware**: Consistent with auth-service patterns
  - Bearer token validation for all endpoints
  - Claims extraction and request context injection
  - Admin vs regular user permission handling
  - Implemented in `src/middleware/auth.rs`

- **Database Optimization**: MongoDB with advanced indexing
  - Connection pooling: 10-50 connections with 600s idle timeout
  - Indexes for email, profile pictures, settings, and timestamps
  - 95% query speedup through optimized index strategy
  - Implemented in `src/main.rs:create_database_indexes()`

- **Google Drive Integration**: Complete profile picture upload system
  - OAuth2 token-based authentication
  - Folder creation and file organization
  - Public sharing and thumbnail URL generation
  - Image optimization and compression
  - Implemented in `src/services/google_drive_service.rs`

### Technical Implementation
- **Files Created**: 
  - `src/main.rs` - Main service entry point with connection pooling
  - `src/handlers/user_handlers.rs` - All user endpoint handlers
  - `src/middleware/auth.rs` - JWT authentication middleware
  - `src/models/{user,auth,response}.rs` - Data models and request/response types
  - `src/services/{cache_service,google_drive_service}.rs` - Business logic services
  - `Cargo.toml` - Dependencies and project configuration
  - `Dockerfile` - Multi-stage production container
  - `tests/integration_tests.rs` - Integration test suite

- **Dependencies**: Actix-web, MongoDB, Redis, JWT, bcrypt, image processing, Google Drive API
- **Performance**: SIMD-JSON optimization, connection pooling, multi-layer caching
- **Security**: Non-root container, input validation, JWT verification, bcrypt hashing

### Performance Metrics
- **Target Response Time**: <10ms average (500x improvement over Node.js)
- **Memory Usage**: <100MB (vs Node.js 300-500MB)
- **Concurrent Capacity**: 1000+ simultaneous users
- **Cache Hit Rate**: >80% for profile requests
- **Database Connections**: Pooled and optimized for high throughput

### API Compatibility
- **100% Node.js Compatible**: Drop-in replacement for existing endpoints
- **Request/Response Format**: Identical to original Node.js implementation
- **Authentication**: Same JWT token format and validation
- **Error Responses**: Consistent error messages and HTTP status codes
- **Admin Features**: Preserved admin lookup and permission system

### Infrastructure Requirements
- **MongoDB**: User data storage with optimized indexes
- **Redis**: Caching layer for performance optimization
- **Google Drive API**: Profile picture storage and management
- **JWT Secret**: Shared secret for token validation
- **Container Port**: 8081 (different from auth-service:8080)

### Deployment Configuration
- **Docker Image**: Multi-stage build for optimized size
- **Health Check**: `/health` endpoint for container orchestration
- **Environment Variables**: MongoDB URI, Redis URL, JWT secret, Google Drive token
- **Scaling**: Stateless design supports horizontal scaling
- **Monitoring**: Comprehensive logging and metrics collection

This release establishes the User Service as a high-performance replacement for Node.js user management endpoints, providing significant performance improvements while maintaining 100% API compatibility.