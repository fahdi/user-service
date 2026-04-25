# CLAUDE.md -- user-service

User profile management, settings, avatar uploads, roles, and admin operations with multi-layer caching.

## Overview

- **Framework**: Actix-web 4.8
- **Port**: 8083 (env: `PORT`, default code says 8081 but Dockerfile exposes 8083)
- **Database**: MongoDB (`isupercoder` db, collections: `users`, `activities`)
- **Cache**: Redis (deadpool-redis) + in-memory LRU (500 profiles, 300 settings)
- **Tests**: 345 tests (201 unit + 49 DI integration + 95 endpoint/security)

## Architecture

```
src/
├── main.rs              # Actix server bootstrap, DB indexes (imports from lib)
├── lib.rs               # All shared modules, global pools, connection init
├── traits.rs            # DI trait abstractions (UserRepository, CacheService, FileUploader, AuthExtractor)
├── impls.rs             # Concrete trait implementations (MongoUserRepository, RedisCacheService, etc.)
├── impls.rs             # Concrete trait implementations (MongoUserRepository, RedisCacheService, etc.)
├── handlers/
│   ├── user_handlers.rs # All route handlers (profile, settings, roles, admin, data export/import)
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
│   ├── rate_limit.rs    # Actix Transform middleware (Redis + LRU fallback)
│   └── mod.rs
└── utils/
    ├── security.rs      # generate_secure_password, validate_email, escape_regex
    └── mod.rs
```

## API Endpoints

### User Profile
- `GET  /health` -- Health check
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
- `GET  /api/users/activity` -- Get user activity log (paginated)

### Data Management
- `GET  /api/users/export` -- Export user data (GDPR compliance)
- `POST /api/users/import` -- Import user data

### Admin
- `GET  /api/admin/users` -- Search/list all users (admin only)
- `PUT  /api/admin/users/{id}` -- Update any user (admin only)

## Key Design Decisions

- **Multi-layer caching**: LRU in-memory (sub-millisecond) -> Redis (shared) -> MongoDB, with TTL-based invalidation
- **Global connection pools**: `lazy_static!` for `REDIS_POOL`, `MONGODB_CLIENT`, `USER_CACHE` -- follows auth-service patterns
- **Image processing**: Profile pictures processed with `image` crate (resize 400x400, crop 200x200 center, JPEG 90% quality)
- **Google Drive storage**: Profile pictures uploaded to user-specific Drive folders, made publicly readable
- **simd-json optimization**: `optimize_json_response()` uses simd-json with serde_json fallback
- **Database indexes**: Created on startup (email unique, profilePicture sparse, updatedAt desc)
- **Rate limiting**: Actix Transform middleware with configurable windows (auth: 10/min, API: 100/min, admin: 200/min)
- **DI traits**: `UserRepository`, `CacheService`, `FileUploader`, `AuthExtractor` in `traits.rs` for testable handlers (fully wired via AppState in main.rs with concrete impls)
- **Node.js API compatibility**: All responses match exact field names from the Node.js predecessor (camelCase)

## Development

```bash
cargo run          # Start on port 8083
cargo test         # Run 201 tests
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
- **simd-json 0.13**: Fast JSON serialization with serde_json fallback
- **jsonwebtoken 9.3**: JWT validation (HS256)
- **lazy_static**: Global static pools and caches

## Testing

- 345 total tests (201 unit + 49 DI integration + 95 endpoint/security)
- DI integration tests (`tests/di_integration_tests.rs`): test all 13 endpoints via trait-based mocks
- `mockall 0.13` available for trait mocking
- Security tests: password generation uniqueness, email validation edge cases, regex escaping
- Auth tests: Claims serialization with camelCase field renames
- Rate limit middleware tests: window-based counting, fallback behavior

## Docker

Multi-stage Debian Slim build. Health check at `/health` via curl on port 8083. Runs as non-root `appuser`. Creates dummy `lib.rs` for dependency caching layer.
