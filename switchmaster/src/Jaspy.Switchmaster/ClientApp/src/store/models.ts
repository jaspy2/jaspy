export enum DeployState {
    Stationed = 0,
}

export interface Switch {
    fqdn: string;
    deployState: DeployState;
    configured: boolean;
}

export interface SynchronizationResult {
    added: number;
    existing: number;
    newSwitches: Switch[];
}

export interface SwitchStoreState {
    items: Switch[];
    syncResult?: SynchronizationResult;
    processing: boolean;
    errors: Error[];
}
