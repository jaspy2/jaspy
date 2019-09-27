FROM debian:buster-slim
WORKDIR /usr/lib/jaspy/discover
COPY discover.py discover.py
COPY lib lib
COPY requirements.txt requirements.txt
RUN apt-get update && apt-get -y install python3-pip python3-venv
RUN python3 -m venv env
RUN /usr/lib/jaspy/discover/env/bin/pip install -r requirements.txt
CMD cp -a /usr/lib/jaspy/discover /output/discover
