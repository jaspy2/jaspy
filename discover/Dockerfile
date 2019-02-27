FROM python:3.6

WORKDIR /usr/src/jaspy-discover
COPY . .
RUN pip install -r requirements.txt --upgrade
CMD ["python", "discover.py", "-s", "$JASPY_SNMPBOT_URL", "-j", "$JASPY_URL", "-c", "$JASPY_COMMUNITY", "-r", "$JASPY_ROOT_DEVICE", "-d", "$JASPY_DNS_DOMAIN", "-D", "-S", "-n"]
