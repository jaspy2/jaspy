FROM nginx:1.15.8-alpine
COPY . /usr/share/nginx/html
COPY ./entrypoint.sh /entrypoint.sh
RUN cp /usr/share/nginx/html/js/config.dist.js /usr/share/nginx/html/js/config.js && \
    chmod +x /entrypoint.sh
ENTRYPOINT ["/entrypoint.sh"]
