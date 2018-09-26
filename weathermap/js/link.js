

export default class Link {
    constructor(name) {
        this.name = name;
        console.log("        create link -> " + name);
    }

    destroy() {
        console.log("        destroy link -> " + this.name);
    }
}