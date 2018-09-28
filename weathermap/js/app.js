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

        this.lastTopologyUpdate = 0;
        this.lastDataUpdate = 0;
        this.lastGraphicsUpdate = 0;

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

    async updateTopologyData() {
        let data = await fetch(config.jaspyNexusURL).then(res => res.json()).catch(fail => Promise.reject(fail));
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

    async dataUpdate() {        
        const curtime = Math.round((new Date()).getTime() / 1000);
        const prometheusDeviceUp = await fetch(config.prometheusQueryURL + 'min(jaspy_device_up{}) by (fqdn)' + '&time=' + curtime).then(res => res.json()).catch(fail => Promise.reject(0));
        if(prometheusDeviceUp["status"] == "error") return Promise.reject(0);
        
        const prometheusInterfaceUp = await fetch(config.prometheusQueryURL + 'min(jaspy_interface_up{neighbors="yes"}) by (fqdn, name)' + '&time=' + curtime).then(res => res.json()).catch(fail => Promise.reject(0));
        if(prometheusInterfaceUp["status"] == "error") return Promise.reject(0);
        
        const prometheusInterfaceSpeed = await fetch(config.prometheusQueryURL + 'min(jaspy_interface_speed{neighbors="yes"}) by (fqdn, name)' + '&time=' + curtime).then(res => res.json()).catch(fail => Promise.reject(0));
        if(prometheusInterfaceSpeed["status"] == "error") return Promise.reject(0);
        
        const prometheusDataOctets = await fetch(config.prometheusQueryURL + 'sum(rate(jaspy_interface_octets{neighbors="yes"}[120s])) by (fqdn, name, direction)' + '&time=' + curtime).then(res => res.json()).catch(fail => Promise.reject(fail));
        if(prometheusDataOctets["status"] == "error") return Promise.reject(0);

        this.beginStatisticsUpdate();
        this.updateInterfaceSpeed(prometheusInterfaceSpeed);
        this.updateInterfaceUp(prometheusInterfaceUp);
        this.updateInterfaceOctetsPerSecond(prometheusDataOctets);
        this.updateDeviceUp(prometheusDeviceUp);
        this.commitStatisticsUpdate();
        this.updateGraphics();
    }

    frame(curtime) {
        if(curtime - this.lastTopologyUpdate > 5) {
            this.lastTopologyUpdate = curtime;
            this.updateTopologyData();
            console.log("topo-tick");
        }
        if(curtime - this.lastDataUpdate > 5) {
            this.lastDataUpdate = curtime;
            this.dataUpdate();
            console.log("data-tick");
        }
    }
}

let wm = new WeatherMap(window.innerWidth, window.innerHeight);
window.addEventListener('resize', function() {
    wm.rendererResize();
});

function mainLoop() {
    const curtime = Math.round((new Date()).getTime() / 1000);
    wm.frame(curtime);
    requestAnimationFrame(mainLoop);
}

mainLoop();