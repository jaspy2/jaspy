
# Jaspy Switchmaster

## Building
A recommended way to build Switchmaster is to use docker with the included `Dockerfile`. Manual steps, without platform specifics, are included for transparency.

### Build with Docker

**Requirements**
* Docker

1. Create a directory you want to place the output artifacts in. eg `/path/to/output`
2. Build to the `builder` of the `Dockerfile`: `docker build . --target builder -t jaspy_switchmaster:builder`
3. Run the built container to extract the published application: `docker run -v /path/to/output:/output jaspy_switchmaster:builder`

> Note that mounting the output artifact folder to `/output` is essential. See `Dockerfile` for *CMD* reference.

### Build manually

**Requirements**
* .NET Core SDK
* Latest NodeJS

After installing the requirements publish `Jaspy.Switchmaster` for your target platform: `dotnet publish --self-contained -r <RuntimeIdentifier> -c <Configuration> -o /app ./src/Jaspy.Switchmaster`

### Example Apache reverse proxy configuration
```
    ProxyPass "/switchmaster/" "http://127.0.0.1:5000/"
    ProxyPassReverse "/switchmaster/" "http://127.0.0.1:5000/"
    
    RewriteEngine on
    RewriteCond %{HTTP:Upgrade} websocket               [NC]
    RewriteRule /hubs/(.*)      ws://localhost:5000/hubs/$1  [P]

    ProxyPass "/hubs/switch" "http://127.0.0.1:5000/hubs/switch"
    ProxyPassReverse "/hubs/switch" "http://127.0.0.1:5000/switch"

    ProxyPass "/hubs" "ws://127.0.0.1:5000/hubs"
    ProxyPassReverse "/hubs" "ws://127.0.0.1:5000/hubs"
```
