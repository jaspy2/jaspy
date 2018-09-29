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
        this.lastStatusUpdate = 0;
        this.lastGraphicsUpdate = 0;
        this.lastAnimationUpdate = 0;

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
        simulationGlobals.viewport = this.viewport;
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
        const data = await fetch(config.jaspyNexusURL).then(res => res.json()).catch(fail => Promise.reject(fail));
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
        // note, this can be used for history browsing 
        let curtime = Math.round((new Date()).getTime() / 1000);
        let timequery = '&time=' + curtime;
        timequery = '';
        const prometheusDeviceUp = await fetch(config.prometheusQueryURL + 'min(jaspy_device_up{}) by (fqdn)' + timequery).then(res => res.json()).catch(fail => Promise.reject(fail));
        if(prometheusDeviceUp["status"] == "error") return Promise.reject(prometheusDeviceUp);
        
        const prometheusInterfaceUp = await fetch(config.prometheusQueryURL + 'min(jaspy_interface_up{neighbors="yes"}) by (fqdn, name)' + timequery).then(res => res.json()).catch(fail => Promise.reject(fail));
        if(prometheusInterfaceUp["status"] == "error") return Promise.reject(prometheusInterfaceUp);
        
        const prometheusInterfaceSpeed = await fetch(config.prometheusQueryURL + 'min(jaspy_interface_speed{neighbors="yes"}) by (fqdn, name)' + timequery).then(res => res.json()).catch(fail => Promise.reject(fail));
        if(prometheusInterfaceSpeed["status"] == "error") return Promise.reject(prometheusInterfaceSpeed);
        
        const prometheusDataOctets = await fetch(config.prometheusQueryURL + 'sum(rate(jaspy_interface_octets{neighbors="yes"}[120s])) by (fqdn, name, direction)' + timequery).then(res => res.json()).catch(fail => Promise.reject(fail));
        if(prometheusDataOctets["status"] == "error") return Promise.reject(prometheusDataOctets);

        this.beginStatisticsUpdate();
        this.updateInterfaceSpeed(prometheusInterfaceSpeed);
        this.updateInterfaceOctetsPerSecond(prometheusDataOctets);
        //these could be used in history-mode
        //this.updateInterfaceUp(prometheusInterfaceUp);
        //this.updateDeviceUp(prometheusDeviceUp);
        this.commitStatisticsUpdate();

        simulationGlobals.animationUpdateRequested = true;
        simulationGlobals.graphicsUpdateRequested = true;
    }

    async statusUpdate() {
        const statusInfo = await fetch(config.jaspyNexusURL+"/state").then(res => res.json()).catch(fail => Promise.reject(fail));
        this.deviceGraph.updateStatuses(statusInfo["devices"]);
    }

    frame(curtime) {
        if(curtime - this.lastTopologyUpdate > 60) {
            this.lastTopologyUpdate = curtime;
            this.updateTopologyData();
            console.log("topo-tick @ " + curtime);
        }
        if(curtime - this.lastDataUpdate > 10) {
            this.lastDataUpdate = curtime;
            this.dataUpdate();
            console.log("data-tick @ " + curtime);
        }
        if(curtime - this.lastStatusUpdate > 1) {
            this.lastStatusUpdate = curtime;
            this.statusUpdate();
            console.log("status-tick @ " + curtime);
        }

        if(curtime - this.lastGraphicsUpdate > 1 || simulationGlobals.requestGraphicsUpdate) {
            this.lastGraphicsUpdate = curtime;
            // reset gfx request flag, might retrigger
            simulationGlobals.requestGraphicsUpdate = false;
            this.updateGraphics();
            console.log("gfx-tick @ " + curtime);
        }

        if(curtime - this.lastAnimationUpdate > 1 || simulationGlobals.requestAnimationUpdate) {
            this.lastAnimationUpdate = curtime;
            // reset anim request flag, might retrigger
            simulationGlobals.requestAnimationUpdate = false;
            // this.updateAnimation();
        }
    }
}

let wm = new WeatherMap(window.innerWidth, window.innerHeight);
window.addEventListener('resize', function() {
    wm.rendererResize();
});

function mainLoop() {
    const curtime = (new Date()).getTime() / 1000.0;
    wm.frame(curtime);
    requestAnimationFrame(mainLoop);
}

mainLoop();