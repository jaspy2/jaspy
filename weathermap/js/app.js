import DeviceGraph from './devicegraph.js';

class WeatherMap extends PIXI.Application {
    constructor(window_width, window_height) {
        super();
        this.frameConstants = {
            loopCount: 0,
            frameNumber: 0,
        };
        
        this.renderer = PIXI.autoDetectRenderer(window_width, window_height, {backgroundColor: 0x111111});
        this.viewportInit();
        this.deviceGraphInit();

        document.body.appendChild(this.renderer.view);
    }

    viewportInit() {
        PIXI.settings.RESOLUTION = 2;
        this.viewport = new PIXI.extras.Viewport({
            screenWidth: window.innerWidth,
            screenHeight: window.innerHeight,        
            interaction: this.renderer.interaction
        });
        this.viewport
            .drag()
            .wheel()
            .decelerate();
        this.stage.addChild(this.viewport);
    }

    deviceGraphInit() {
        this.deviceGraph = new DeviceGraph(this.viewport);
    }

    rendererResize() {
        this.viewport.screenHeight = window.innerHeight;
        this.viewport.screenWidth = window.innerWidth;
        this.renderer.resize(window.innerWidth, window.innerHeight);
        this.viewport.resize();
    }

    updateTopologyData(data) {
        this.deviceGraph.updateTopologyData(data);
    }

    updateGraphics() {
        this.deviceGraph.updateGraphics();
    }

    beginStatisticsUpdate() {
        this.deviceGraph.beginStatisticsUpdate();
    }

    commitStatisticsUpdate() {
        this.deviceGraph.commitStatisticsUpdate();
    }

    updateInterfaceOctetsPerSecond(prometheusData) {
        if(prometheusData["status"] === "success") {
            if(prometheusData["data"]["resultType"] !== "vector") {
                console.error("prometheus query returned unexpected data");
                console.error(prometheusData["data"]);
            }
            this.deviceGraph.updateInterfaceOctetsPerSecond(prometheusData["data"]["result"]);
        } else {
            console.error("prometheus query failed");
            console.error(prometheusData);
        }
    }

    updateInterfaceSpeed(prometheusData) {
        if(prometheusData["status"] === "success") {
            if(prometheusData["data"]["resultType"] !== "vector") {
                console.error("prometheus query returned unexpected data");
                console.error(prometheusData["data"]);
            }
            this.deviceGraph.updateInterfaceSpeed(prometheusData["data"]["result"]);
        } else {
            console.error("prometheus query failed");
            console.error(prometheusData);
        }
    }

    updateInterfaceUp(prometheusData) {
        if(prometheusData["status"] === "success") {
            if(prometheusData["data"]["resultType"] !== "vector") {
                console.error("prometheus query returned unexpected data");
                console.error(prometheusData["data"]);
            }
            this.deviceGraph.updateInterfaceUp(prometheusData["data"]["result"]);
        } else {
            console.error("prometheus query failed");
            console.error(prometheusData);
        }
    }

    updateDeviceUp(prometheusData) {
        if(prometheusData["status"] === "success") {
            if(prometheusData["data"]["resultType"] !== "vector") {
                console.error("prometheus query returned unexpected data");
                console.error(prometheusData["data"]);
            }
            this.deviceGraph.updateDeviceUp(prometheusData["data"]["result"]);
        } else {
            console.error("prometheus query failed");
            console.error(prometheusData);
        }
    }
}

let wm = new WeatherMap(window.innerWidth, window.innerHeight);
window.addEventListener('resize', function() {
    wm.rendererResize();
});

let exampleData = {"devices":{"esxg.test.makai.fi":{"fqdn":"esxg.test.makai.fi","interfaces":{"0/2":{"name":"0/2","ifIndex":2,"connectedTo":null},"0/1":{"name":"0/1","ifIndex":1,"connectedTo":null},"0/6":{"name":"0/6","ifIndex":6,"connectedTo":null},"0/4":{"name":"0/4","ifIndex":4,"connectedTo":null},"0/15":{"name":"0/15","ifIndex":15,"connectedTo":{"fqdn":"nege.test.makai.fi","interface":"g8"}},"0/16":{"name":"0/16","ifIndex":16,"connectedTo":null},"3/4":{"name":"3/4","ifIndex":29,"connectedTo":null},"3/3":{"name":"3/3","ifIndex":28,"connectedTo":null},"0/12":{"name":"0/12","ifIndex":12,"connectedTo":{"fqdn":"tenshi.test.makai.fi","interface":"ixgbe0"}},"0/11":{"name":"0/11","ifIndex":11,"connectedTo":null},"0/5":{"name":"0/5","ifIndex":5,"connectedTo":null},"CPU Interface:  5/1":{"name":"CPU Interface:  5/1","ifIndex":25,"connectedTo":null},"0/14":{"name":"0/14","ifIndex":14,"connectedTo":null},"0/9":{"name":"0/9","ifIndex":9,"connectedTo":null},"3/5":{"name":"3/5","ifIndex":30,"connectedTo":null},"0/8":{"name":"0/8","ifIndex":8,"connectedTo":null},"0/13":{"name":"0/13","ifIndex":13,"connectedTo":null},"3/6":{"name":"3/6","ifIndex":31,"connectedTo":null},"3/2":{"name":"3/2","ifIndex":27,"connectedTo":null},"0/10":{"name":"0/10","ifIndex":10,"connectedTo":null},"0/7":{"name":"0/7","ifIndex":7,"connectedTo":null},"3/1":{"name":"3/1","ifIndex":26,"connectedTo":null},"0/3":{"name":"0/3","ifIndex":3,"connectedTo":null}}},"tenshi.test.makai.fi":{"fqdn":"tenshi.test.makai.fi","interfaces":{"rt-lo0":{"name":"rt-lo0","ifIndex":9,"connectedTo":null},"vif-netbox":{"name":"vif-netbox","ifIndex":15,"connectedTo":null},"sit0":{"name":"sit0","ifIndex":16,"connectedTo":null},"vif-ubntwlc":{"name":"vif-ubntwlc","ifIndex":13,"connectedTo":null},"eth2":{"name":"eth2","ifIndex":4,"connectedTo":null},"tap-blacklight":{"name":"tap-blacklight","ifIndex":18,"connectedTo":null},"lo":{"name":"lo","ifIndex":1,"connectedTo":null},"ixgbe0":{"name":"ixgbe0","ifIndex":2,"connectedTo":{"fqdn":"esxg.test.makai.fi","interface":"0/12"}},"ixgbe1":{"name":"ixgbe1","ifIndex":5,"connectedTo":null},"dummy0":{"name":"dummy0","ifIndex":8,"connectedTo":null},"vlan10":{"name":"vlan10","ifIndex":7,"connectedTo":null},"6rd":{"name":"6rd","ifIndex":17,"connectedTo":null},"eth1":{"name":"eth1","ifIndex":3,"connectedTo":null},"vlan100":{"name":"vlan100","ifIndex":6,"connectedTo":null}}},"nege.test.makai.fi":{"fqdn":"nege.test.makai.fi","interfaces":{"g3":{"name":"g3","ifIndex":3,"connectedTo":null},"g9":{"name":"g9","ifIndex":9,"connectedTo":null},"l1":{"name":"l1","ifIndex":14,"connectedTo":null},"g6":{"name":"g6","ifIndex":6,"connectedTo":null},"g5":{"name":"g5","ifIndex":5,"connectedTo":null},"g1":{"name":"g1","ifIndex":1,"connectedTo":null},"g2":{"name":"g2","ifIndex":2,"connectedTo":null},"g4":{"name":"g4","ifIndex":4,"connectedTo":null},"g7":{"name":"g7","ifIndex":7,"connectedTo":null},"l4":{"name":"l4","ifIndex":17,"connectedTo":null},"g10":{"name":"g10","ifIndex":10,"connectedTo":null},"g8":{"name":"g8","ifIndex":8,"connectedTo":{"fqdn":"esxg.test.makai.fi","interface":"0/15"}},"l2":{"name":"l2","ifIndex":15,"connectedTo":null},"cpu":{"name":"cpu","ifIndex":13,"connectedTo":null},"l3":{"name":"l3","ifIndex":16,"connectedTo":null}}}}};
let prometheusDataOctets = {"status":"success","data":{"resultType":"vector","result":[{"metric":{"direction":"tx","fqdn":"tenshi.test.makai.fi","name":"ixgbe0"},"value":[1538139300,"1211199035.44927536232"]},{"metric":{"direction":"rx","fqdn":"esxg.test.makai.fi","name":"0/12"},"value":[1538139300,"129103.06666666667"]},{"metric":{"direction":"rx","fqdn":"esxg.test.makai.fi","name":"0/15"},"value":[1538139300,"12652.625120772947"]},{"metric":{"direction":"rx","fqdn":"nege.test.makai.fi","name":"g8"},"value":[1538139300,"14159.392753623188"]},{"metric":{"direction":"rx","fqdn":"tenshi.test.makai.fi","name":"ixgbe0"},"value":[1538139300,"100137.53140096618"]},{"metric":{"direction":"tx","fqdn":"esxg.test.makai.fi","name":"0/12"},"value":[1538139300,"130479.52608695652"]},{"metric":{"direction":"tx","fqdn":"esxg.test.makai.fi","name":"0/15"},"value":[1538139300,"13991.871014492754"]},{"metric":{"direction":"tx","fqdn":"nege.test.makai.fi","name":"g8"},"value":[1538139300,"12899.350724637681"]}]}};
let prometheusInterfaceSpeed = {"status":"success","data":{"resultType":"vector","result":[{"metric":{"fqdn":"nege.test.makai.fi","name":"g8"},"value":[1538140455.683,"1000"]},{"metric":{"fqdn":"tenshi.test.makai.fi","name":"ixgbe0"},"value":[1538140455.683,"10000"]},{"metric":{"fqdn":"esxg.test.makai.fi","name":"0/12"},"value":[1538140455.683,"10000"]},{"metric":{"fqdn":"esxg.test.makai.fi","name":"0/15"},"value":[1538140455.683,"1000"]}]}};
let prometheusInterfaceUp = {"status":"success","data":{"resultType":"vector","result":[{"metric":{"fqdn":"esxg.test.makai.fi","name":"0/12"},"value":[1538141363.868,"1"]},{"metric":{"fqdn":"esxg.test.makai.fi","name":"0/15"},"value":[1538141363.868,"1"]},{"metric":{"fqdn":"nege.test.makai.fi","name":"g8"},"value":[1538141363.868,"1"]},{"metric":{"fqdn":"tenshi.test.makai.fi","name":"ixgbe0"},"value":[1538141363.868,"1"]}]}};
let prometheusDeviceUp = {"status":"success","data":{"resultType":"vector","result":[{"metric":{"fqdn":"esxg.test.makai.fi"},"value":[1538143687.328,"1"]},{"metric":{"fqdn":"nege.test.makai.fi"},"value":[1538143687.328,"1"]},{"metric":{"fqdn":"tenshi.test.makai.fi"},"value":[1538143687.328,"1"]}]}};
wm.updateTopologyData(exampleData);

wm.beginStatisticsUpdate();
wm.updateInterfaceSpeed(prometheusInterfaceSpeed);
wm.updateInterfaceUp(prometheusInterfaceUp);
wm.updateInterfaceOctetsPerSecond(prometheusDataOctets);
wm.updateDeviceUp(prometheusDeviceUp);
wm.commitStatisticsUpdate();
wm.updateGraphics();
//wm.updateTopologyData(exampleData2);