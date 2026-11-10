//! Program-owned token accounts at PDA addresses: the fee vaults
//! (`["fee_vault", payment_mint]`) and the deal escrow vaults
//! (`["deal_vault", deal]`).
//!
//! They are created here rather than with an Anchor `token::` `init` because
//! the mint (and, for deal vaults, which of the two mints) is only known after
//! the handler's checks have run.

use anchor_lang::{prelude::*, solana_program::program_pack::Pack, system_program};
use anchor_spl::{
    token_2022::spl_token_2022::{
        extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions},
        state::{Account as TokenAccountState, Mint as MintState},
    },
    token_interface::{initialize_account3, InitializeAccount3},
};

/// Accounts needed to create one PDA token account.
pub struct PdaTokenAccount<'a, 'info> {
    pub payer: &'a AccountInfo<'info>,
    /// The PDA address; must be signable with `signer_seeds`.
    pub account: &'a AccountInfo<'info>,
    pub mint: &'a AccountInfo<'info>,
    /// Owner (authority) of the new token account.
    pub authority: &'a AccountInfo<'info>,
    pub token_program: Pubkey,
    pub system_program: Pubkey,
}

/// Creates `account` as a token account of `mint` owned by `authority`: what an
/// Anchor `token::` `init` does, but callable after the handler's checks. Like
/// Anchor's `init`, it tolerates lamports sent to the address beforehand
/// (anyone can do that): it then tops up, allocates and assigns instead of
/// `create_account`, which would fail on a funded address.
pub fn create_pda_token_account(
    accounts: PdaTokenAccount,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let PdaTokenAccount {
        payer,
        account,
        mint,
        authority,
        token_program,
        system_program: system,
    } = accounts;

    let space = token_account_space(mint)?;
    let rent = Rent::get()?.minimum_balance(space);
    let current = account.lamports();
    if current == 0 {
        system_program::create_account(
            CpiContext::new_with_signer(
                system,
                system_program::CreateAccount {
                    from: payer.clone(),
                    to: account.clone(),
                },
                signer_seeds,
            ),
            rent,
            space as u64,
            &token_program,
        )?;
    } else {
        let top_up = rent.saturating_sub(current);
        if top_up > 0 {
            system_program::transfer(
                CpiContext::new(
                    system,
                    system_program::Transfer {
                        from: payer.clone(),
                        to: account.clone(),
                    },
                ),
                top_up,
            )?;
        }
        system_program::allocate(
            CpiContext::new_with_signer(
                system,
                system_program::Allocate {
                    account_to_allocate: account.clone(),
                },
                signer_seeds,
            ),
            space as u64,
        )?;
        system_program::assign(
            CpiContext::new_with_signer(
                system,
                system_program::Assign {
                    account_to_assign: account.clone(),
                },
                signer_seeds,
            ),
            &token_program,
        )?;
    }
    initialize_account3(CpiContext::new(
        token_program,
        InitializeAccount3 {
            account: account.clone(),
            mint: mint.clone(),
            authority: authority.clone(),
        },
    ))
}

/// Size of a token account for `mint`, including the account extensions its
/// mint extensions require (same computation as Anchor's `init`). Only called
/// on mints that passed the allowlist, so every extension type is known.
fn token_account_space(mint: &AccountInfo) -> Result<usize> {
    if *mint.owner != anchor_spl::token_2022::ID {
        return Ok(TokenAccountState::LEN);
    }
    let data = mint.try_borrow_data()?;
    let state = StateWithExtensions::<MintState>::unpack(&data)?;
    let mint_extensions = state.get_extension_types()?;
    let required = ExtensionType::get_required_init_account_extensions(&mint_extensions);
    Ok(ExtensionType::try_calculate_account_len::<TokenAccountState>(&required)?)
}
