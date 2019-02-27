create table switches (
    id bigserial primary key,
    display_name text not null check (char_length(display_name) < 100),
    configured boolean not null default false,
    deploy_state text not null default 'stationed'
);
