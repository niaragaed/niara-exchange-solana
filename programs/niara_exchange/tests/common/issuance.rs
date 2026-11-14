//! Issuance fixtures: mint authority PDA, batches, burns.

use super::*;
use niara_exchange::{
    BurnReason, IssuanceBatch, Listing, ListingStatus, BATCH_SEED, MINT_AUTHORITY_SEED,
};

pub const ISSUANCE_DOC: [u8; 32] = [9u8; 32];
pub const BURN_REF: [u8; 32] = [5u8; 32];

pub fn mint_authority_pda(asset_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[MINT_AUTHORITY_SEED, asset_mint.as_ref()], &program_id()).0
}

pub fn batch_pda(asset_mint: &Pubkey, batch_no: u64) -> Pubkey {
    let listing = listing_pda(asset_mint);
    Pubkey::find_program_address(
        &[BATCH_SEED, listing.as_ref(), &batch_no.to_le_bytes()],
        &program_id(),
    )
    .0
}

impl Env {
    /// Base mint layout (SPL Token and Token-2022): `supply` at offset 36.
    pub fn mint_supply(&self, mint: &Pubkey) -> u64 {
        let data = self.svm.get_account(mint).unwrap().data;
        u64::from_le_bytes(data[36..44].try_into().unwrap())
    }

    /// Mint base layout: `mint_authority` is a `COption<Pubkey>` at offset 0.
    pub fn mint_authority(&self, mint: &Pubkey) -> Option<Pubkey> {
        let data = self.svm.get_account(mint).unwrap().data;
        (data[0..4] == [1, 0, 0, 0]).then(|| Pubkey::try_from(&data[4..36]).unwrap())
    }

    pub fn set_mint_supply(&mut self, mint: &Pubkey, supply: u64) {
        let mut account = self.svm.get_account(mint).unwrap();
        account.data[36..44].copy_from_slice(&supply.to_le_bytes());
        self.svm.set_account(*mint, account).unwrap();
    }

    /// Writes `status` into the listing account, bypassing the instructions.
    /// For states the program no longer lets a listing reach with a batch
    /// open (e.g. `Quoted` with a `Requested` batch).
    pub fn force_listing_status(&mut self, asset_mint: &Pubkey, status: ListingStatus) {
        use anchor_lang::AccountSerialize;
        let mut listing = self.listing(asset_mint);
        listing.status = status;
        let mut data = Vec::new();
        listing.try_serialize(&mut data).unwrap();
        let address = listing_pda(asset_mint);
        let mut account = self.svm.get_account(&address).unwrap();
        account.data[..data.len()].copy_from_slice(&data);
        self.svm.set_account(address, account).unwrap();
    }

    pub fn listing(&self, asset_mint: &Pubkey) -> Listing {
        self.fetch(&listing_pda(asset_mint))
    }

    pub fn batch(&self, asset_mint: &Pubkey, batch_no: u64) -> IssuanceBatch {
        self.fetch(&batch_pda(asset_mint, batch_no))
    }

    /// Requests the listing's next batch (the number is read from the listing).
    pub fn request_issuance(
        &mut self,
        signer: &Keypair,
        asset_mint: Pubkey,
        amount: u64,
        recipient: Pubkey,
        doc_hash: [u8; 32],
    ) -> TransactionResult {
        let batch_no = self.listing(&asset_mint).next_batch_no;
        self.request_issuance_at(signer, asset_mint, batch_no, amount, recipient, doc_hash)
    }

    /// Same, with an explicit batch PDA number (to test out-of-sequence numbers).
    pub fn request_issuance_at(
        &mut self,
        signer: &Keypair,
        asset_mint: Pubkey,
        batch_no: u64,
        amount: u64,
        recipient: Pubkey,
        doc_hash: [u8; 32],
    ) -> TransactionResult {
        let ix = ix(
            accounts::RequestIssuance {
                issuer: signer.pubkey(),
                listing: listing_pda(&asset_mint),
                batch: batch_pda(&asset_mint, batch_no),
                system_program: system_program::ID,
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::RequestIssuance {
                amount,
                recipient,
                doc_hash,
            },
        );
        self.send(&[ix], &[signer])
    }

    pub fn approve_issuance(
        &mut self,
        signer: &Keypair,
        asset_mint: Pubkey,
        batch_no: u64,
        destination: Pubkey,
    ) -> TransactionResult {
        let ix = ix(
            accounts::ApproveIssuance {
                listing_authority: signer.pubkey(),
                config: config_pda(),
                listing: listing_pda(&asset_mint),
                batch: batch_pda(&asset_mint, batch_no),
                asset_mint,
                mint_authority: mint_authority_pda(&asset_mint),
                destination,
                token_program: self.owner_of(&asset_mint),
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::ApproveIssuance {},
        );
        self.send(&[ix], &[signer])
    }

    pub fn reject_issuance(
        &mut self,
        signer: &Keypair,
        asset_mint: Pubkey,
        batch_no: u64,
    ) -> TransactionResult {
        let ix = ix(
            accounts::RejectIssuance {
                listing_authority: signer.pubkey(),
                config: config_pda(),
                listing: listing_pda(&asset_mint),
                batch: batch_pda(&asset_mint, batch_no),
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::RejectIssuance {},
        );
        self.send(&[ix], &[signer])
    }

    pub fn burn_with_reason(
        &mut self,
        holder: &Keypair,
        asset_mint: Pubkey,
        holder_account: Pubkey,
        amount: u64,
        reason: BurnReason,
        reference: [u8; 32],
    ) -> TransactionResult {
        let ix = ix(
            accounts::BurnWithReason {
                holder: holder.pubkey(),
                listing: listing_pda(&asset_mint),
                asset_mint,
                holder_account,
                token_program: self.owner_of(&asset_mint),
                event_authority: event_authority(),
                program: program_id(),
            },
            instruction::BurnWithReason {
                amount,
                reason,
                reference,
            },
        );
        self.send(&[ix], &[holder])
    }

    /// Requests and approves one batch to a fresh account of `recipient`;
    /// returns that account. The listing must be `Approved`.
    pub fn issue(
        &mut self,
        issuer: &Keypair,
        asset_mint: Pubkey,
        recipient: Pubkey,
        amount: u64,
    ) -> Pubkey {
        let program = self.owner_of(&asset_mint);
        let account = self.create_token_account(program, asset_mint, recipient, 0);
        let batch_no = self.listing(&asset_mint).next_batch_no;
        self.request_issuance(issuer, asset_mint, amount, recipient, ISSUANCE_DOC)
            .unwrap();
        let authority = self.listing_authority.insecure_clone();
        self.approve_issuance(&authority, asset_mint, batch_no, account)
            .unwrap();
        account
    }

    /// Invariant 11 with no direct burns: supply == issued - burned.
    pub fn assert_supply_matches_issuance(&self, asset_mint: &Pubkey) {
        let listing = self.listing(asset_mint);
        assert_eq!(
            self.mint_supply(asset_mint),
            listing.issued - listing.burned,
            "supply != issued - burned (issued {}, burned {})",
            listing.issued,
            listing.burned
        );
    }
}
