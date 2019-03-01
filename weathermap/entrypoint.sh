#!/bin/sh
sed -i 's~JASPY_NEXUS_URL~'$JASPY_NEXUS_URL'~g' /usr/share/nginx/html/js/config.js
sed -i 's~JASPY_PROMETHEUS_URL~'$JASPY_PROMETHEUS_URL'~g' /usr/share/nginx/html/js/config.js
/usr/sbin/nginx -g 'daemon off;'
