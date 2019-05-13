# jaspy

## Deployment

### Docker
Install [Docker](https://docs.docker.com/install/) and
[docker-compose](https://docs.docker.com/compose/install/)
##### Clone repo
```
git clone https://github.com/jaspy2/jaspy.git
```
##### Modify config
```
cd jaspy
$EDITOR jaspy.env
```
##### Run compose
```
docker-compose up -d  
```

### Debian package from sources

#### Install necessary build-dependencies (stretch)
```
apt install libpq-dev libzmq3-dev liboping-dev libtool

# Install golang-go if you need to build snmpbot (you probably do)
apt install -t stretch-backports golang-go
```

#### Clone and build jaspy deb-package
```
git clone https://github.com/jaspy2/jaspy.git
cd jaspy/debian
./build.sh

# If you are building snmpbot, proceed to run following
cd snmpbot
./build.sh
```

