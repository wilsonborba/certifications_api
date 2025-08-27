
#!/usr/bin/env bash
openssl rand 32 | base64 | tr '+/' '-_'