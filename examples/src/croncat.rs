// This example will go over the croncat contracts found
// [here](https://github.com/CronCats/contracts/blob/cafd3caafb91b45abb6e811ce0fa2819980d6f96/manager/src/lib.rs)
// This will demonstrate a more involved example for fast-forwarding the blockchain to a future
// state/time. This is useful for testing anything that is time dependent such as scheduling
// This is perfect to showcase cron.cat which will schedule calling into contract functions
// at a set amount of time we supply.

use near_gas::NearGas;
use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde::Deserialize;
use serde_json::json;

const MANAGER_CONTRACT: &[u8] = include_bytes!("../res/manager.wasm");
const COUNTER_CONTRACT: &[u8] = include_bytes!("../res/counter.wasm");

/// `AgentStatus` struct taken from [croncat repo](github.com/CronCats/contracts/) to
/// deserialize into after we get the result of a transaction and converting over to
/// this particular type.
#[derive(Debug, Deserialize, PartialEq, Eq)]
pub enum AgentStatus {
    Active,
    Pending,
}

/// `Agent` struct taken from [croncat repo](github.com/CronCats/contracts/) to deserialize
/// into after we get the result of a transaction and converting over to this particular type.
/// Helpful for understanding what our output is from a contract call. For a more in depth
/// look at what an `Agent` is all about, refer to the [croncat docs](https://docs.cron.cat/docs/)
/// to understand further, but for this example all we care about is that an Agent is something
/// that can run scheduled tasks once it is time and collect rewards thereafter.
#[derive(Debug, Deserialize)]
pub struct Agent {
    pub status: AgentStatus,
    pub payable_account_id: AccountId,
    // NOTE: display_fromstr is used to deserialize from a U128 type returned from the contract
    // which is represented as a string there, and then converted into a rust u128 here.
    pub balance: NearToken,
    #[serde(with = "serde_with::rust::display_fromstr")]
    pub total_tasks_executed: u128,
    pub last_missed_slot: u128,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Spawn sandbox as normal and get us a local blockchain for us to interact and toy with:
    let worker = near_workspaces::sandbox().await?;

    // Initialize counter contract, which will be pointed to in the manager contract to schedule
    // a task later to increment the counter, inside counter contract.
    let counter_contract = worker.dev_deploy(COUNTER_CONTRACT).await?;

    // deploy the manager contract so we can schedule tasks via our agents.
    let manager_contract = worker.dev_deploy(MANAGER_CONTRACT).await?;
    manager_contract
        .call("new")
        .transact()
        .await?
        .into_result()?;

    // Create a root croncat account with agent subaccounts to schedule tasks.
    let croncat = worker.dev_create_account().await?;

    // This will setup a task to call into the counter contract, with a cadence of 1 hour.
    println!("Creating task for `counter.increment`");
    let outcome = croncat
        .call(manager_contract.id(), "create_task")
        .args_json(json!({
            "contract_id": counter_contract.id(),
            "function_id": "increment",
            "cadence": "*/1 * * * * *",
            "recurring": true,
        }))
        .max_gas()
        .deposit(NearToken::from_near(1))
        .transact()
        .await?;
    println!("-- outcome: {:#?}\n", outcome);

    // Let's create an agent that will eventually execute the above task and get rewards
    // for executing it:
    let agent_1 = croncat
        .create_subaccount("agent_1")
        .initial_balance(NearToken::from_near(10))
        .transact()
        .await?
        .into_result()?;

    // Now with all the above setup complete, we can now have the agent run our task:
    run_scheduled_tasks(&worker, &manager_contract, &agent_1).await?;

    Ok(())
}

/// This function will schedule a particular task (`counter.increment`) and a single agent
/// will run that task to eventually get rewards.
pub async fn run_scheduled_tasks(
    worker: &Worker<Sandbox>,
    contract: &Contract,
    agent: &Account,
) -> anyhow::Result<()> {
    // Register the agent to eventually execute the task
    let outcome = agent
        .call(contract.id(), "register_agent")
        .args_json(json!({}))
        .deposit(NearToken::from_yoctonear(2260000000000000000000u128))
        .transact()
        .await?;
    println!("Registering agent outcome: {:#?}\n", outcome);

    // Check the right agent was registered correctly:
    let registered_agent = contract
        .call("get_agent")
        .args_json(json!({ "account_id": agent.id() }))
        .view()
        .await?
        .json::<Option<Agent>>()?
        .unwrap();
    println!("Registered agent details: {:#?}", registered_agent);
    assert_eq!(registered_agent.status, AgentStatus::Active);
    assert_eq!(&registered_agent.payable_account_id, agent.id());

    // Advance 4500 blocks in the chain. 1 block takes approx 1.5 seconds to be produced, but we
    // don't actually wait that long since we are time travelling to the future via `fast_forward`!
    // After this `fast_forward` call, we should be ahead by about an hour, and it is expected for
    // our agents to be able to execute the task we scheduled.
    println!("Waiting until next slot occurs...");
    worker.fast_forward(4500).await?;

    // Quick proxy call to earn a reward. Essentially telling the agent to execute the task
    // if it can. The time based conditions are checked right in the contract. We are in the future
    // here, so the agent should be executing the task.
    agent
        .call(contract.id(), "proxy_call")
        .gas(NearGas::from_tgas(200))
        .transact()
        .await?
        .into_result()?;

    // Do it again, just to show that this can be done multiple times since our task is a
    // recurring one that happens every hour:
    worker.fast_forward(4500).await?;
    agent
        .call(contract.id(), "proxy_call")
        .gas(NearGas::from_gas(200))
        .transact()
        .await?
        .into_result()?;

    // Check accumulated agent balance after completing our task. This value is held within
    // the manager contract, and we want to eventually withdraw this amount.
    let agent_details = contract
        .call("get_agent")
        .args_json(json!({"account_id": agent.id()}))
        .view()
        .await?
        .json::<Option<Agent>>()?
        .unwrap();
    println!("Agent details after completing task: {:#?}", agent_details);
    assert_eq!(
        agent_details.balance,
        NearToken::from_yoctonear(3860000000000000000000u128)
    );
    let before_withdraw = agent_details.balance;

    // Withdraw the reward from completing the task to our agent's account
    agent
        .call(contract.id(), "withdraw_task_balance")
        .transact()
        .await?
        .into_result()?;

    // Check accumulated agent balance to see that the amount has been taken out of the manager
    // contract:
    let agent_details = contract
        .call("get_agent")
        .args_json(json!({"account_id": agent.id() }))
        .view()
        .await?
        .json::<Option<Agent>>()?
        .unwrap();
    println!("Agent details after withdrawing task: {:#?}", agent_details);
    assert_eq!(
        agent_details.balance,
        NearToken::from_yoctonear(2260000000000000000000u128)
    );

    // This shows how much the agent has profitted from executing the task:
    println!(
        "Agent profitted {} yN and has been transferred to the agent's account",
        before_withdraw.as_yoctonear() - agent_details.balance.as_yoctonear()
    );

    // Not that everything is done, let's cleanup and unregister the agent from doing anything.
    agent
        .call(contract.id(), "unregister_agent")
        .deposit(NearToken::from_yoctonear(1))
        .transact()
        .await?
        .into_result()?;

    // Check to see if the agent has been successfully unregistered
    let removed_agent: Option<Agent> = contract
        .call("get_agent")
        .args_json(json!({ "account_id": agent.id() }))
        .view()
        .await?
        .json()?;
    assert!(
        removed_agent.is_none(),
        "Agent should have been removed via `unregister_agent`"
    );

    Ok(())
}
