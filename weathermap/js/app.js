import DeviceGraph from './devicegraph.js';

class WeatherMap extends PIXI.Application {
    constructor(window_width, window_height) {
        super();

        this.ticker.autoStart = false;
        this.ticker.stop();

        this.frameConstants = {
            loopCount: 0,
            frameNumber: 0,
        };

        PIXI.settings.PRECISION_FRAGMENT = 'highp';
        this.renderer = PIXI.autoDetectRenderer(window_width, window_height, {
            backgroundColor: 0x111111,
            antialias: false,
        });
        this.renderer.roundPixels = true;

        this.viewportInit();
        this.deviceGraphInit();

        this.lastTopologyUpdate = 0;
        this.lastDataUpdate = 0;
        this.lastStatusUpdate = 0;
        this.lastGraphicsUpdate = 0;
        this.lastAnimationUpdate = 0;
        this.lastPositionUpdate = 0;

        document.body.appendChild(this.renderer.view);
        this.bindDeviceSearch();
    }

    bindDeviceSearch() {
        let deviceGraph = this.deviceGraph;
        document.getElementById("devicesearch").addEventListener("submit", function(ev) {
            ev.preventDefault();
            let devices = deviceGraph.findDevicesByName(ev.target[0].value);
            devices.forEach(function(d) { d.highlight(); });
        });
    }

    viewportInit() {
        this.viewport = new PIXI.extras.Viewport({
            screenWidth: window.innerWidth,
            screenHeight: window.innerHeight,
            interaction: this.renderer.interaction
        });
        this.viewport
            .drag()
            .wheel()
            .decelerate();
        this.viewport.on('moved', function() {
            simulationGlobals.requestPIXIFrame = true;
        });
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

    updateAnimation() {
        this.deviceGraph.updateAnimation();
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

    async positionUpdate() {
        const positionInfo = await fetch(config.jaspyNexusURL+"/position").then(res => res.json()).catch(fail => Promise.reject(fail));
        this.deviceGraph.updatePositions(positionInfo["devices"]);
        simulationGlobals.animationUpdateRequested = true;
        simulationGlobals.graphicsUpdateRequested = true;
    }

    frame(curtime) {
        let tickerUpdate = false;

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
        if(curtime - this.lastPositionUpdate > 1 && !simulationGlobals.positionUpdatesFrozen) {
            this.lastPositionUpdate = curtime;
            this.positionUpdate();
            console.log("position-tick @ " + curtime);
        }

        if(curtime - this.lastGraphicsUpdate > 10 || simulationGlobals.requestGraphicsUpdate) {
            this.lastGraphicsUpdate = curtime;
            // reset gfx request flag, might retrigger
            simulationGlobals.requestGraphicsUpdate = false;
            this.updateGraphics();
            tickerUpdate = true;
            console.log("gfx-tick @ " + curtime);
        }

        if(curtime - this.lastAnimationUpdate > 10 || simulationGlobals.requestAnimationUpdate) {
            this.lastAnimationUpdate = curtime;
            // reset anim request flag, might retrigger
            simulationGlobals.requestAnimationUpdate = false;
            this.updateAnimation();
            tickerUpdate = true;
            console.log("simulation-tick @ " + curtime);
        }

        if(simulationGlobals.requestPIXIFrame) {
            tickerUpdate = true;
            simulationGlobals.requestPIXIFrame = false;
        }

        if(tickerUpdate) wm.ticker.update();
    }
}

let wm = new WeatherMap(window.innerWidth, window.innerHeight);
window.addEventListener('resize', function() {
    wm.rendererResize();
});

function mainLoop() {
    let curtime = (new Date()).getTime() / 1000.0;
    wm.frame(curtime);
    requestAnimationFrame(mainLoop);
}

mainLoop();
