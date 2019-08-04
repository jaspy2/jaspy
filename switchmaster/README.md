    ProxyPass "/switchmaster/" "http://127.0.0.1:5000/"
    ProxyPassReverse "/switchmaster/" "http://127.0.0.1:5000/"

    ProxyPass "/hubs/switch" "http://127.0.0.1:5000/hubs/switch"
    ProxyPassReverse "/hubs/switch" "http://127.0.0.1:5000/switch"

    ProxyPass "/hubs" "ws://127.0.0.1:5000/hubs"
    ProxyPassReverse "/hubs" "ws://127.0.0.1:5000/hubs"