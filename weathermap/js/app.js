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
}

let wm = new WeatherMap(window.innerWidth, window.innerHeight);
window.addEventListener('resize', function() {
    wm.rendererResize();
});

let exampleData = {
    "devices": {
        "testdata1.domain.tld": {
            "interfaces": {
                "Te1/1": {
                    "name": "Te1/1",
                    "ifIndex": 1,
                    "connectedTo": {
                        "fqdn": "testdata2.domain.tld",
                        "interface": "Te9/1"
                    }
                }
            }
        },
        "testdata2.domain.tld": {
            "interfaces": {
                "Te9/1": {
                    "name": "Te9/1",
                    "ifIndex": 1,
                    "connectedTo": {
                        "fqdn": "testdata1.domain.tld",
                        "interface": "Te1/1"
                    }
                },
                "Te9/2": {
                    "name": "Te9/2",
                    "ifIndex": 1,
                    "connectedTo": {
                        "fqdn": "testdata3.domain.tld",
                        "interface": "Te2/1"
                    }
                }
            }
        },
        "testdata3.domain.tld": {
            "interfaces": {
                "Te2/1": {
                    "name": "Te2/1",
                    "ifIndex": 1,
                    "connectedTo": {
                        "fqdn": "testdata2.domain.tld",
                        "interface": "Te9/2"
                    }
                }
            }
        },
    }
}

wm.updateTopologyData(exampleData);
wm.updateTopologyData(exampleData);