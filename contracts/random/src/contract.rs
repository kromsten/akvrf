use cosmwasm_std::{
    entry_point, 
    to_binary, 
    Binary, 
    Deps, 
    DepsMut, 
    Env, 
    MessageInfo, 
    Response, 
    StdResult
};

use rand_chacha::{
    ChaChaRng, 
    rand_core::{
        SeedableRng, CryptoRngCore
    }
};
use secret_toolkit::permit::Permit;


use crate::{
    msg::{ExecuteMsg, QueryMsg, IBCLifecycleComplete, SudoMsg, InstantiateMsg, MainPageResponse}, 
    random::{try_saving_random_number, get_saved_random_number, randomness_seed}, error::ContractError,
    ibc::{ibc_lifecycle_complete, ibc_timeout}, 
    state::{CellState, CELLS, Config, CONFIG, FIELD_SIZE, NETWORK_CONFIGS}, 
    field::{valid_field_size, try_opening_cell, get_field_cells}, utils::{address_from_permit, is_powerup_list_unique, is_chain_id_list_unique}, admin::{forwards_funds, set_app_status}, powerups::{try_buying_powerups, get_user_powerups}, networks::{get_all_network_configs, get_network_config}
};


pub const ONE_DAY : u64 = 24 * 3600;


#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {

    // field size
    let field_size = msg.field_size.unwrap_or(64);
    if !valid_field_size(field_size) {
        return Err(ContractError::InvalidFieldSize {});
    }
    
    FIELD_SIZE.save(deps.storage, &field_size)?;

    // win amounts

    let mut chain_ids: Vec<String> = Vec::with_capacity(msg.network_configs.len());

    for (denom, configs) in msg.network_configs.iter() {

        chain_ids.push(configs.chain_id.clone());

        let powerup_list = configs.power_ups
                .iter().map(|(powerup, _)| powerup.clone()).collect::<Vec<_>>();

        if configs.power_ups.len() != 3 || !is_powerup_list_unique(&powerup_list) {
            return Err(ContractError::InvalidPowerupAmounts {});
        }

        if denom.starts_with("ibc/") && configs.channel_id.is_none() {
            return Err(ContractError::MissingChannelId {});
        }

        NETWORK_CONFIGS.insert(deps.storage, &denom, &configs).unwrap();
    }

    if !is_chain_id_list_unique(&chain_ids) {
        return Err(ContractError::DuplicateChainIds {});
    }

    // config
    let cell_cooldown = msg.cell_cooldown.unwrap_or(2*ONE_DAY);
    let user_cooldown = msg.user_cooldown.unwrap_or(ONE_DAY);
    // default around 4%
    let win_threshold = msg.win_threshold.unwrap_or(u8::MAX as u16 * 2 - 20u16);


    CONFIG.save(deps.storage, &Config { 
        win_threshold, 
        cell_cooldown, 
        user_cooldown,
    })?;

    let mut ring = ChaChaRng::from_seed(
        randomness_seed(&env.block, info.sender.as_str())
    );
    let generator = ring.as_rngcore();
    for i in 1..(field_size+1) {
        CELLS.insert(deps.storage, &i, &CellState {
            random: (generator.next_u32() % u8::MAX as u32) as u8,
            open_at: env.block.time.seconds()
        })?
    }


    deps.api
        .debug(format!("Contract was initialized by {}", info.sender).as_str());


    Ok(Response::default())
}


#[entry_point]
pub fn execute(deps: DepsMut, env: Env, info: MessageInfo, msg: ExecuteMsg) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::UpdateMyRandomNumber { 
            permit 
        } => try_saving_random_number(deps, env, permit),

        ExecuteMsg::OpenCell { 
            permit,
            cell_id,
            powerups,
            power_up_autopay
        } => {

            let sender = address_from_permit(deps.as_ref(), &env, &permit)?;
                
            try_opening_cell(
                deps, 
                env, 
                sender,
                cell_id, 
                powerups, 
                power_up_autopay,
                info.funds
            )
        },

        ExecuteMsg::BuyPowerups { 
            permit,
            powerups,
        } => {
            let sender = address_from_permit(deps.as_ref(), &env, &permit)?;
            try_buying_powerups(
                deps, 
                sender,
                powerups,
                info.funds
            )
        },

        ExecuteMsg::SetAppStatus { status } => set_app_status(deps, info.sender, status),

        
        ExecuteMsg::ForwardsFunds {
            to_address,
            amount,
        } => forwards_funds(deps.as_ref(), info.sender, to_address, amount),


        ExecuteMsg::IBCLifecycleComplete(IBCLifecycleComplete::IBCAck {
            channel,
            sequence,
            ack,
            success,
        }) => ibc_lifecycle_complete(channel, sequence, ack, success),

        ExecuteMsg::IBCLifecycleComplete(IBCLifecycleComplete::IBCTimeout { 
            channel, 
            sequence 
        }) => ibc_timeout(channel, sequence)
    }
}



#[entry_point]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::GetField {} => to_binary(&get_field_cells(deps)?),
        QueryMsg::GetMyRandomNumber { permit } => to_binary(&get_saved_random_number(deps, env, permit)?),
        QueryMsg::GetMyPowerups { permit } => to_binary(&get_user_powerups(deps, env, permit)?),
        QueryMsg::NetworkConfig { denom } => to_binary(&get_network_config(deps, denom)),
        QueryMsg::AllNetworkConfigs {} => to_binary(&get_all_network_configs(deps)?),

        QueryMsg::Main { permit } => to_binary(&get_main(deps, env, permit)?),
    }
}


#[entry_point]
pub fn sudo(_deps: DepsMut, _env: Env, msg: SudoMsg) -> Result<Response, ContractError> {
    match msg {
        SudoMsg::IBCLifecycleComplete(IBCLifecycleComplete::IBCAck {
            channel,
            sequence,
            ack,
            success,
        }) => ibc_lifecycle_complete(channel, sequence, ack, success),

        SudoMsg::IBCLifecycleComplete(IBCLifecycleComplete::IBCTimeout {
            channel,
            sequence,
        }) => ibc_timeout(channel, sequence),
    }
}


pub fn get_main(
    deps: Deps, 
    env: Env, 
    permit: Option<Permit>
) -> StdResult<MainPageResponse> {
    let field_res = get_field_cells(deps)?;

    let powerups = if permit.is_some() {
        Some(get_user_powerups(deps, env, permit.unwrap())?)
    } else {
        None
    };
    
    let network_configs = get_all_network_configs(deps)?;
    Ok(MainPageResponse {
        cells: field_res.cells,
        powerups,
        network_configs
    })
}