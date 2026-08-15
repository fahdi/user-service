# CLAUDE.md -- user-service

User profile management, settings, avatar uploads, roles, and admin operations with multi-layer caching.

## Overview

- **Framework**: Actix-web 4.8
- **Port**: 8083 (env: `PORT`, default code says 8081 but Dockerfile exposes 8083)
- **Database**: MongoDB (`isupercoder` db, collections: `users`, `activities`)
- **Cache**: Redis (deadpool-redis) + in-memory LRU (500 profiles, 300 settings)
- **Tests**: ~476 tests (exact count lives in CI)

## Architecture

```
src/
├── main.rs              # Actix server bootstrap, DB indexes (imports from lib)
├── lib.rs               # All shared modules, global pools, connection init
├── traits.rs            # DI trait abstractions (UserRepository, CacheService, FileUploader, AuthExtractor)
├── impls.rs             # Concrete trait implementations (MongoUserRepository, RedisCacheService, etc.)
├── impls.rs             # Concrete trait implementations (MongoUserRepository, RedisCacheService, etc.)
├── handlers/
│   ├── di_handlers.rs   # All route handlers (profile, settings, roles, admin, data export/import) - trait-based DI
│   ├── helpers.rs       # Pure helper functions (standardization, pagination, validation)
│   ├── di_handlers.rs   # Trait-based handler variants for DI testing
│   └── mod.rs
├── models/
│   ├── user.rs          # CachedUserProfile, StandardizedUser, UserSettings, request/response types
│   ├── auth.rs          # JWT Claims struct (camelCase serde: userId, type)
│   ├── response.rs      # ErrorResponse, SuccessResponse wrappers
│   └── mod.rs
├── services/
│   ├── cache_service.rs # Multi-layer caching: LRU -> Redis -> DB (profile + settings)
│   ├── google_drive_service.rs # Profile picture upload via Google Drive API
│   └── mod.rs
├── middleware/
│   ├── auth.rs          # JWT extraction from Authorization header (HS256)
│   └── mod.rs
└── utils/
    ├── security.rs      # generate_secure_password, validate_email, escape_regex
    └── mod.rs
```

## API Endpoints

### User Profile
- `GET  /health` -- Health check. Pings MongoDB and returns **503** when it is unreachable (issue #42). The cache is deliberately excluded: `CacheService` returns `Option`, so a failure is indistinguishable from a miss and falls through to the database
- `GET  /api/users/profile` -- Get authenticated user's profile (or another user via query)
- `POST /api/users/profile-picture` -- Upload profile picture (multipart, resized to 200x200 JPEG)
- `DELETE /api/users/avatar` -- Delete profile picture

### Settings
- `GET  /api/users/settings` -- Get user settings
- `PUT  /api/users/settings` -- Update user settings

### Account
- `POST /api/users/change-password` -- Change password (bcrypt hashing)

### Roles & Activity
- `GET  /api/users/roles` -- Get user role definitions and permissions
- `PUT  /api/users/roles` -- Update user role (admin only)
- `GET  /api/users/activity` -- Reads the `user_activities` collection, paginated. This service writes it too (#70): `profile_updated` / `settings_updated` from `update_settings`, `password_changed` from `change_password`, `avatar_updated` / `avatar_deleted` from `update_profile_picture` / `delete_avatar`, and `role_changed` from `update_user_role` and `admin_update_user` (only when a role was actually part of the request). Project, file, and message events are deliberately out of scope - those belong to the services that own them. A write failure never fails the request it describes: `record_activity` in `di_handlers.rs` logs the error and returns, so a broken activity log cannot 502 a password change

### Data Management
- `GET  /api/users/export` -- Export user data (GDPR compliance)
- `POST /api/users/import` -- Import user data

### Admin
- `GET  /api/admin/users` -- Search/list all users (admin only)
- `POST /api/admin/users` -- Create a user account (admin only). No admin-chosen password: the handler generates one and bcrypt-hashes it, the account starts unverified, and the response says a reset is required (#85). Duplicate email is a 409, distinguishing a Mongo `E11000` write error from any other repository failure
- `PUT  /api/admin/users/{id}` -- Update any user (admin only)

## Key Design Decisions

- **Multi-layer caching**: LRU in-memory (sub-millisecond) -> Redis (shared) -> MongoDB, with TTL-based invalidation
- **Global connection pools**: `lazy_static!` for `REDIS_POOL`, `MONGODB_CLIENT`, `USER_CACHE` -- follows auth-service patterns
- **Image processing**: Profile pictures processed with `image` crate (resize 400x400, crop 200x200 center, JPEG 90% quality)
- **Google Drive storage**: Profile pictures uploaded to user-specific Drive folders, made publicly readable
- **Database indexes**: Created on startup (email unique, profilePicture sparse, updatedAt desc)
- **No rate limiting**: this service has none; abuse throttling happens at auth-service's per-endpoint limits. (A never-compiled Transform implementation was removed in #26.)
- **DI traits**: `UserRepository`, `CacheService`, `FileUploader`, `AuthExtractor` in `traits.rs` for testable handlers (fully wired via AppState in main.rs with concrete impls)
- **Node.js API compatibility**: All responses match exact field names from the Node.js predecessor (camelCase)

## Development

```bash
cargo run          # Start on port 8083
cargo test         # Run the full suite (~476 tests)
cargo clippy       # Zero warnings required
```

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| PORT | No | 8081 | Server port |
| MONGODB_URI | Yes | - | MongoDB connection string (panics if missing) |
| REDIS_URL | No | redis://127.0.0.1:6379 | Redis connection |
| JWT_SECRET | Yes | - | JWT signing secret (panics if missing) |
| GOOGLE_DRIVE_ACCESS_TOKEN | No | - | OAuth2 token for Drive uploads |

## Dependencies

- **actix-web 4.8**: HTTP framework
- **actix-multipart 0.7**: File upload handling
- **mongodb 2.8 / bson 2.9**: Database driver
- **deadpool-redis 0.15**: Connection pooling for Redis
- **lru 0.12**: In-memory LRU cache (500 profiles, 300 settings entries)
- **bcrypt 0.15**: Password hashing
- **image 0.24**: Profile picture processing (resize, crop, JPEG encode)
- **reqwest 0.11**: Google Drive API HTTP client
- **validator 0.16**: Input validation with derive macros
- **jsonwebtoken 9.3**: JWT validation (HS256)
- **lazy_static**: Global static pools and caches

## Testing

- ~476 tests (exact count lives in CI) across unit, DI integration, and endpoint/security suites
- **`tests/di_integration_tests.rs` is the suite that exercises production handlers.** It imports them from `user_service::handlers::di_handlers` and drives all 14 endpoints through trait-based mocks. If you are checking whether a handler's behaviour is actually pinned, this is the file to read.
- **`tests/handler_integration_tests.rs` does not.** It is 3038 lines that define **13 handlers locally**, mirroring the production set by name, and test those copies; its crate imports are models, traits and one helper, not the handlers under test. Every production equivalent is covered in `di_integration_tests.rs`, so this is redundancy rather than a gap - but a copy of the handler layer will drift from it, and the filename does not say which of the two is authoritative (#75).
- `tests/integration_tests.rs` was deleted in #75: four of its five tests contained no assertion (`let _ = serde_json::json!(..)` under comments saying "verify crate compiles"), and the fifth defined a `health_handler` inside the test file that always returned `"healthy"` and asserted it returned `"healthy"`. That was actively misleading here, because the real handler returns **503** when MongoDB is unreachable and a passing `test_health_endpoint` gave no reason to look for the test that pins it (`health_tests::unreachable_database_is_a_503`).
- `mockall 0.13` available for trait mocking
- Security tests: password generation uniqueness, email validation edge cases, regex escaping
- Auth tests: Claims serialization with camelCase field renames

## Docker

Multi-stage Debian Slim build. Health check at `/health` via curl on port 8083. Runs as non-root `appuser`. Creates dummy `lib.rs` for dependency caching layer.
