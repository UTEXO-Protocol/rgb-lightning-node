/**
 * GENERATED FILE — do not edit by hand.
 *
 * RLN API types for the endpoint subset the gateway uses, generated from the
 * repo-root openapi.yaml. Regenerate with:
 *   pnpm --filter @utexo/minimal-gateway generate:rln-types
 */
export interface paths {
    "/nodeinfo": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Get node info
         * @description Get the LN node's info
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["NodeInfoResponse"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/address": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Get a Bitcoin address
         * @description Get a new Bitcoin address from the internal BDK wallet
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["AddressResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/btcbalance": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Get the BTC balance
         * @description Get the node's bitcoin balance for the vanilla and colored wallets
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["BtcBalanceRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["BtcBalanceResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sendbtc": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Send BTC
         * @description Send bitcoins on-chain
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["SendBtcRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["SendBtcResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/createutxos": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create UTXOs
         * @description Create UTXOs to be used for RGB operations
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["CreateUtxosRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["EmptyResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/rgbinvoice": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Get an RGB invoice
         * @description Get an RGB invoice to receive assets on-chain
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["RgbInvoiceRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["RgbInvoiceResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/decodergbinvoice": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Decode an RGB invoice
         * @description Decode the provided RGB invoice string
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["DecodeRGBInvoiceRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["DecodeRGBInvoiceResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sendrgb": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Send RGB assets
         * @description Send RGB assets on-chain, supporting batch transfers to multiple recipients and/or multiple assets in a single transaction
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["SendRgbRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["SendRgbResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/listtransfers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * List transfers
         * @description List the node's RGB transfers, ordered most-recent first, scoped by an asset filter and optionally by txid. Supports index-based cursor pagination (idx) and optional filtering by status and creation time interval.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["ListTransfersRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["ListTransfersResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/refreshtransfers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Refresh transfers
         * @description Refresh RGB pending transfers
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["RefreshRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["RefreshResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/lninvoice": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Get a LN invoice
         * @description Get a LN invoice to receive a payment. Provide `payment_hash` to create a HODL invoice. Provide `description` to include a BOLT11 description, or `description_hash` to include an out-of-band BOLT11 description hash (mutually exclusive). Provide `min_final_cltv_expiry_delta` to request an explicit inbound final CLTV policy.
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["LNInvoiceRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["LNInvoiceResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/decodelninvoice": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Decode a LN invoice
         * @description Decode the provided LN invoice string
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["DecodeLNInvoiceRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["DecodeLNInvoiceResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/invoicestatus": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Get an invoice status
         * @description Get the status of the provided LN invoice
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["InvoiceStatusRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["InvoiceStatusResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sendpayment": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Send a payment
         * @description Pay the provided LN invoice
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["SendPaymentRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["SendPaymentResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/getpayment": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Get a payment
         * @description Get a payment by its payment hash
         */
        post: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: {
                content: {
                    "application/json": components["schemas"]["GetPaymentRequest"];
                };
            };
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["GetPaymentResponse"];
                    };
                };
            };
        };
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/listpayments": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List payments
         * @description List the node's LN payments (inbound and outbound) ordered most-recent first. Supports index-based cursor pagination: pass the last_index_offset of a page as the next index_offset to fetch the next, older page. Optionally filter by status, direction (inbound/outbound) and creation time interval.
         */
        get: {
            parameters: {
                query?: {
                    /** @description Exclusive upper-bound cursor: only payments with a lower index are returned. 0 or absent means start from the most recent payment. */
                    index_offset?: number;
                    /** @description Maximum number of payments to return. Defaults to 100 when absent or zero. */
                    max_payments?: number;
                    /** @description Return only payments with this HTLC status. */
                    status?: "Pending" | "Succeeded" | "Failed" | "Claimable" | "Claiming" | "Cancelled";
                    /** @description Return only inbound or only outbound payments. */
                    direction?: "Inbound" | "Outbound";
                    /** @description Return only payments created at or after this Unix timestamp (seconds). */
                    created_after?: number;
                    /** @description Return only payments created at or before this Unix timestamp (seconds). */
                    created_before?: number;
                };
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["ListPaymentsResponse"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/listchannels": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List channels
         * @description List the node's LN channels
         */
        get: {
            parameters: {
                query?: never;
                header?: never;
                path?: never;
                cookie?: never;
            };
            requestBody?: never;
            responses: {
                /** @description Successful operation */
                200: {
                    headers: {
                        [name: string]: unknown;
                    };
                    content: {
                        "application/json": components["schemas"]["ListChannelsResponse"];
                    };
                };
            };
        };
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        AddressResponse: {
            /** @example bcrt1qnc5y6j6dmejrkwy93farhvpezk0lf46gk7aecs */
            address: string;
        };
        AssetFilter: components["schemas"]["AssetFilterAnyOrNone"] | components["schemas"]["AssetFilterNone"] | components["schemas"]["AssetFilterId"];
        AssetFilterAnyOrNone: {
            /**
             * @description discriminator enum property added by openapi-typescript
             * @enum {string}
             */
            type: "AnyOrNone";
        };
        /**
         * @example {
         *       "type": "Id",
         *       "value": "rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8"
         *     }
         */
        AssetFilterId: {
            /**
             * @description discriminator enum property added by openapi-typescript
             * @enum {string}
             */
            type: "Id";
            /** @example rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8 */
            value: string;
        };
        AssetFilterNone: {
            /**
             * @description discriminator enum property added by openapi-typescript
             * @enum {string}
             */
            type: "None";
        };
        /** @enum {string} */
        AssetSchema: "Nia" | "Uda" | "Cfa" | "Ifa";
        Assignment: components["schemas"]["AssignmentFungible"] | components["schemas"]["AssignmentNonFungible"] | components["schemas"]["AssignmentInflationRight"] | components["schemas"]["AssignmentAny"];
        AssignmentAny: {
            /**
             * @description discriminator enum property added by openapi-typescript
             * @enum {string}
             */
            type: "Any";
        };
        /**
         * @example {
         *       "type": "Fungible",
         *       "value": 42
         *     }
         */
        AssignmentFungible: {
            /**
             * @description discriminator enum property added by openapi-typescript
             * @enum {string}
             */
            type: "Fungible";
            /** @example 42 */
            value: number;
        };
        AssignmentInflationRight: {
            /**
             * @description discriminator enum property added by openapi-typescript
             * @enum {string}
             */
            type: "InflationRight";
            /** @example 200 */
            value: number;
        };
        AssignmentNonFungible: {
            /**
             * @description discriminator enum property added by openapi-typescript
             * @enum {string}
             */
            type: "NonFungible";
        };
        /**
         * @example Regtest
         * @enum {string}
         */
        BitcoinNetwork: "Mainnet" | "Testnet" | "Testnet4" | "Signet" | "SignetCustom" | "Regtest";
        BtcBalance: {
            /** @example 777000 */
            settled: number;
            /** @example 777000 */
            future: number;
            /** @example 777000 */
            spendable: number;
        };
        BtcBalanceRequest: {
            /** @example false */
            skip_sync: boolean;
        };
        BtcBalanceResponse: {
            vanilla: components["schemas"]["BtcBalance"];
            colored: components["schemas"]["BtcBalance"];
        };
        Channel: {
            /** @example 8129afe1b1d7cf60d5e1bf4c04b09bec925ed4df5417ceee0484e24f816a105a */
            channel_id: string;
            /** @example 5a106a814fe28404eece1754dfd45e92ec9bb0044cbfe1d560cfd7b1e1af2981 */
            funding_txid?: string | null;
            /** @example 03b79a4bc1ec365524b4fab9a39eb133753646babb5a1da5c4bc94c53110b7795d */
            peer_pubkey: string;
            /** @example null */
            peer_alias?: string | null;
            /** @example 120946279120896 */
            short_channel_id?: number | null;
            status: components["schemas"]["ChannelStatus"];
            /** @example false */
            ready: boolean;
            /** @example 30010 */
            capacity_sat: number;
            /** @example 28616 */
            local_balance_sat: number;
            /** @example 21616000 */
            outbound_balance_msat: number;
            /** @example 6394000 */
            inbound_balance_msat: number;
            /** @example 3001000 */
            next_outbound_htlc_limit_msat: number;
            /** @example 1 */
            next_outbound_htlc_minimum_msat: number;
            /** @example false */
            is_usable: boolean;
            /** @example true */
            public: boolean;
            /** @example rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8 */
            asset_id?: string | null;
            /** @example 777 */
            asset_local_amount?: number | null;
            /** @example 0 */
            asset_remote_amount?: number | null;
            /** @example trusted_no_broadcast */
            virtual_open_mode?: string;
        };
        /** @enum {string} */
        ChannelStatus: "Opening" | "Opened" | "Closing";
        CreateUtxosRequest: {
            /** @example false */
            up_to: boolean;
            /** @example 4 */
            num?: number | null;
            /** @example 32500 */
            size?: number | null;
            /** @example 5 */
            fee_rate: number;
            /** @example false */
            skip_sync: boolean;
        };
        DecodeLNInvoiceRequest: {
            /** @example lnbcrt30u1pjv6yzndqud3jxktt5w46x7unfv9kz6mn0v3jsnp4qdpc280eur52luxppv6f3nnj8l6vnd9g2hnv3qv6mjhmhvlzf6327pp5tjjasx6g9dqptea3fhm6yllq5wxzycnnvp8l6wcq3d6j2uvpryuqsp5l8az8x3g8fe05dg7cmgddld3da09nfjvky8xftwsk4cj8p2l7kfq9qyysgqcqpcxqzdylzlwfnkyw3jv344x4rzwgkk53ng0fhxy5rdduk4g5tpvea8xa6rfckkza35va28xjn2tqkhgarcxep5umm4x5k56wfcdvu95eq7qzp20vrl4xz76syapsa3c09j7lg5gerkaj63llj0ark7ph8hfketn6fkqzm8laf66dhsncm23wkwm5l5377we9e8lnlknnkwje5eefkccusqm6rqt8 */
            invoice: string;
        };
        DecodeLNInvoiceResponse: {
            /** @example 3000000 */
            amt_msat?: number | null;
            /** @example 420 */
            expiry_sec: number;
            /** @example 1691160659 */
            timestamp: number;
            /** @example rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8 */
            asset_id?: string | null;
            /** @example 42 */
            asset_amount?: number | null;
            /** @example 1 cup of coffee */
            description?: string | null;
            /** @example 5ca5d81b482b4015e7b14df7a27fe0a38c226273604ffd3b008b752571811938 */
            description_hash?: string | null;
            /** @example 5ca5d81b482b4015e7b14df7a27fe0a38c226273604ffd3b008b752571811938 */
            payment_hash: string;
            /** @example f9fa239a283a72fa351ec6d0d6fdb16f5e59a64cb10e64add0b57123855ff592 */
            payment_secret: string;
            /** @example 0343851df9e0e8aff0c10b3498ce723ff4c9b4a855e6c8819adcafbbb3e24ea2af */
            payee_pubkey?: string | null;
            /** @example 144 */
            min_final_cltv_expiry_delta: number;
            network: components["schemas"]["BitcoinNetwork"];
        };
        DecodeRGBInvoiceRequest: {
            /** @example rgb:icfqnK9y-wObZKTu-XJcDL98-sKbE5Mh-OuDJhiI-brRJrzE/RWhwUfTMpuP2Zfx1~j4nswCANGeJrYOqDcKelaMV4zU/~/bcrt:utxob:cbgHUJ4e-7QyKY4U-Jsj5AZw-oI0gxZh-7fxQY2_-tFFUAZN-4CgpX?expiry=1749906951&endpoints=rpcs://proxy.iriswallet.com/0.2/json-rpc */
            invoice: string;
        };
        DecodeRGBInvoiceResponse: {
            /** @example bcrt:utxob:cbgHUJ4e-7QyKY4U-Jsj5AZw-oI0gxZh-7fxQY2_-tFFUAZN-4CgpX */
            recipient_id: string;
            /** @example bcrt:utxob:cbgHUJ4e-7QyKY4U-Jsj5AZw-oI0gxZh-7fxQY2_-tFFUAZN-4CgpY */
            proxy_recipient_id: string;
            recipient_type: components["schemas"]["RecipientType"];
            asset_schema?: components["schemas"]["AssetSchema"] | null;
            /** @example rgb:icfqnK9y-wObZKTu-XJcDL98-sKbE5Mh-OuDJhiI-brRJrzE */
            asset_id?: string | null;
            assignment: components["schemas"]["Assignment"];
            network: components["schemas"]["BitcoinNetwork"];
            /** @example 1698325849 */
            expiration_timestamp?: number | null;
            transport_endpoints: string[];
            /** @example {} */
            unknown_query_params: {
                [key: string]: string;
            };
        };
        EmptyResponse: Record<string, never>;
        GetPaymentRequest: {
            /** @example 5ca5d81b482b4015e7b14df7a27fe0a38c226273604ffd3b008b752571811938 */
            payment_hash: string;
            payment_type: components["schemas"]["PaymentType"];
        };
        GetPaymentResponse: {
            payment: components["schemas"]["Payment"];
        };
        /** @enum {string} */
        HTLCStatus: "Pending" | "Claimable" | "Claiming" | "Succeeded" | "Cancelled" | "Failed";
        /** @enum {string} */
        InvoiceStatus: "Pending" | "Claimable" | "Claiming" | "Succeeded" | "Cancelled" | "Failed" | "Expired";
        InvoiceStatusRequest: {
            /** @example lnbcrt30u1pjv6yzndqud3jxktt5w46x7unfv9kz6mn0v3jsnp4qdpc280eur52luxppv6f3nnj8l6vnd9g2hnv3qv6mjhmhvlzf6327pp5tjjasx6g9dqptea3fhm6yllq5wxzycnnvp8l6wcq3d6j2uvpryuqsp5l8az8x3g8fe05dg7cmgddld3da09nfjvky8xftwsk4cj8p2l7kfq9qyysgqcqpcxqzdylzlwfnkyw3jv344x4rzwgkk53ng0fhxy5rdduk4g5tpvea8xa6rfckkza35va28xjn2tqkhgarcxep5umm4x5k56wfcdvu95eq7qzp20vrl4xz76syapsa3c09j7lg5gerkaj63llj0ark7ph8hfketn6fkqzm8laf66dhsncm23wkwm5l5377we9e8lnlknnkwje5eefkccusqm6rqt8 */
            invoice: string;
        };
        InvoiceStatusResponse: {
            status: components["schemas"]["InvoiceStatus"];
        };
        LNInvoiceRequest: {
            /** @example 3000000 */
            amt_msat?: number | null;
            /** @example 420 */
            expiry_sec: number;
            /** @example rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8 */
            asset_id?: string | null;
            /** @example 42 */
            asset_amount?: number | null;
            /**
             * @description Optional. When provided, the invoice is created as HODL.
             * @example 3febfae1e68b190c15461f4c2a3290f9af1dae63fd7d620d2bd61601869026cd
             */
            payment_hash?: string;
            /**
             * @description Optional. When provided, the invoice includes a BOLT11 description. Mutually exclusive with description_hash.
             * @example 1 cup of coffee
             */
            description?: string;
            /**
             * @description Optional. When provided, the invoice includes a BOLT11 description hash. Mutually exclusive with description.
             * @example 5ca5d81b482b4015e7b14df7a27fe0a38c226273604ffd3b008b752571811938
             */
            description_hash?: string;
            /**
             * @description Optional. Requested inbound final CLTV policy in blocks.
             * @example 144
             */
            min_final_cltv_expiry_delta?: number;
        };
        LNInvoiceResponse: {
            /** @example lnbcrt30u1pjv6yzndqud3jxktt5w46x7unfv9kz6mn0v3jsnp4qdpc280eur52luxppv6f3nnj8l6vnd9g2hnv3qv6mjhmhvlzf6327pp5tjjasx6g9dqptea3fhm6yllq5wxzycnnvp8l6wcq3d6j2uvpryuqsp5l8az8x3g8fe05dg7cmgddld3da09nfjvky8xftwsk4cj8p2l7kfq9qyysgqcqpcxqzdylzlwfnkyw3jv344x4rzwgkk53ng0fhxy5rdduk4g5tpvea8xa6rfckkza35va28xjn2tqkhgarcxep5umm4x5k56wfcdvu95eq7qzp20vrl4xz76syapsa3c09j7lg5gerkaj63llj0ark7ph8hfketn6fkqzm8laf66dhsncm23wkwm5l5377we9e8lnlknnkwje5eefkccusqm6rqt8 */
            invoice: string;
        };
        ListChannelsResponse: {
            channels: components["schemas"]["Channel"][];
        };
        ListPaymentsResponse: {
            payments: components["schemas"]["Payment"][];
            /**
             * Format: uint64
             * @description Index of the first (most recent) payment in the returned page, or 0 when the page is empty.
             * @example 100
             */
            first_index_offset: number;
            /**
             * Format: uint64
             * @description Index of the last (oldest) payment in the returned page, or 0 when the page is empty. Pass this as index_offset to fetch the next, older page.
             * @example 51
             */
            last_index_offset: number;
        };
        /** @description asset_filter selects the asset scope: Id (that asset's transfers), None (only transfers not tied to an asset) or AnyOrNone (no asset restriction). txid further restricts the result to the transfers committed by that on-chain transaction, across all assets; combined with an Id filter it acts as an intersection. AnyOrNone without a txid is rejected, so narrow by asset_filter, by txid, or by both. */
        ListTransfersRequest: {
            asset_filter: components["schemas"]["AssetFilter"];
            /**
             * @description Return the transfers committed by the on-chain transaction with this txid. Combined with an Id asset_filter it acts as an intersection.
             * @example 47ee0f5b7bd5b0dd7f10ce54a94fee1b5cd54e5241b0f70f9f373d10e7a3c3e2
             */
            txid?: string | null;
            /**
             * Format: uint64
             * @description Exclusive upper-bound cursor: only transfers with a lower idx are returned. 0 or absent means start from the most recent.
             */
            index_offset?: number | null;
            /**
             * Format: uint64
             * @description Maximum number to return. Defaults to 100 when absent or zero.
             */
            max_transfers?: number | null;
            /**
             * @description Return only transfers with this status.
             * @enum {string|null}
             */
            status?: "Initiated" | "WaitingCounterparty" | "WaitingSafeHeight" | "WaitingConfirmations" | "Settled" | "Failed" | null;
            /**
             * Format: uint64
             * @description Return only transfers created at or after this Unix timestamp (seconds).
             */
            created_after?: number | null;
            /**
             * Format: uint64
             * @description Return only transfers created at or before this Unix timestamp (seconds).
             */
            created_before?: number | null;
        };
        ListTransfersResponse: {
            transfers: components["schemas"]["Transfer"][];
            /**
             * Format: uint64
             * @description Index (idx) of the first transfer in the page, or 0 when empty.
             */
            first_index_offset: number;
            /**
             * Format: uint64
             * @description Index (idx) of the last transfer in the page, or 0 when empty. Pass as index_offset to fetch the next, older page.
             */
            last_index_offset: number;
        };
        NodeInfoResponse: {
            /** @example 02270dadcd6e7ba0ef707dac72acccae1a3607453a8dd2aef36ff3be4e0d31f043 */
            pubkey: string;
            /** @example 1 */
            num_channels: number;
            /** @example 0 */
            num_usable_channels: number;
            /** @example 28616 */
            local_balance_sat: number;
            /** @example 892 */
            eventual_close_fees_sat: number;
            /** @example 7852 */
            pending_outbound_payments_sat: number;
            /** @example 1 */
            num_peers: number;
            /** @example tpubDDfzqHEET3ksD81qshMHkw35yp6TuLP1kr5rWWeJcLAqDfMXKDJzmDwAnda6DCqw7kkkhPphuDZFE2a6Sw8h5ZA5NwmtTssEnjMqN7xMzSd */
            account_xpub_vanilla: string;
            /** @example tpubDDcdKhaxwVV2T6xwigti7dSY1a7LHFwZmKAaLWtNhzrvuTXqjjzo8U7YQkUuPah5yHvnk3cbXmb18ZRFwHEKTFUQmA9dij1nPVA2LCJCiEa */
            account_xpub_colored: string;
            /** @example 5 */
            max_media_upload_size_mb: number;
            /** @example 3000000 */
            rgb_htlc_min_msat: number;
            /** @example 30010 */
            rgb_channel_capacity_min_sat: number;
            /** @example 5506 */
            channel_capacity_min_sat: number;
            /** @example 16777215 */
            channel_capacity_max_sat: number;
            /** @example 1 */
            channel_asset_min_amount: number;
            /** @example 18446744073709552000 */
            channel_asset_max_amount: number;
            /** @example 987226 */
            network_nodes: number;
            /** @example 7812821 */
            network_channels: number;
            /**
             * @description Unix timestamp of the most recently applied Rapid Gossip Sync snapshot. Null when the node sources gossip via P2P or has not yet applied an RGS snapshot.
             * @example 1715731200
             */
            latest_rgs_snapshot_timestamp?: number | null;
        };
        Payment: {
            /** @example 3000000 */
            amt_msat?: number | null;
            /** @example 42 */
            asset_amount?: number | null;
            /** @example rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8 */
            asset_id?: string | null;
            /** @example 3febfae1e68b190c15461f4c2a3290f9af1dae63fd7d620d2bd61601869026cd */
            payment_hash: string;
            payment_type: components["schemas"]["PaymentType"];
            status: components["schemas"]["HTLCStatus"];
            /** @example 1691160765 */
            created_at: number;
            /** @example 1691162674 */
            updated_at: number;
            /** @example 03b79a4bc1ec365524b4fab9a39eb133753646babb5a1da5c4bc94c53110b7795d */
            payee_pubkey: string;
            /** @example 89d28bd306aa9bb906fd0ac31092d04c37c919a171b343083167e2a3cdc60578 */
            preimage?: string;
            /**
             * @description BOLT11 description (the d tag), if present
             * @example 1 cup of coffee
             */
            description?: string | null;
            /**
             * @description BOLT11 description hash (the h tag), hex-encoded, if present
             * @example 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
             */
            description_hash?: string | null;
        };
        /** @enum {string} */
        PaymentType: "Outbound" | "InboundAutoClaim" | "InboundHodl";
        Recipient: {
            /** @example bcrt:utxob:2FZsSuk-iyVQLVuU4-Gc6J4qkE8-mLS17N4jd-MEx6cWz9F-MFkyE1n */
            recipient_id: string;
            witness_data?: components["schemas"]["WitnessData"] | null;
            assignment: components["schemas"]["Assignment"];
            transport_endpoints: string[];
        };
        /** @enum {string} */
        RecipientType: "Blind" | "Witness";
        RefreshFailure: {
            /** @example InvalidConsignment */
            name: string;
            /** @example Invalid consignment */
            message: string;
        };
        RefreshFilter: {
            status: components["schemas"]["RefreshTransferStatus"];
            incoming: boolean;
        };
        RefreshRequest: {
            /** @example rgb:2dkSTbr-jFhznbPmo-TQafzswCN-av4gTsJjX-ttx6CNou5-M98k8Zd */
            asset_id?: string | null;
            /** @example [] */
            filter: components["schemas"]["RefreshFilter"][];
            /** @example false */
            skip_sync: boolean;
        };
        RefreshResponse: {
            transfers: {
                [key: string]: components["schemas"]["RefreshedTransfer"];
            };
        };
        /** @enum {string} */
        RefreshTransferStatus: "WaitingCounterparty" | "WaitingConfirmations";
        RefreshedTransfer: {
            /**
             * @example WaitingBroadcast
             * @enum {string|null}
             */
            updated_status?: "Initiated" | "WaitingCounterparty" | "WaitingSafeHeight" | "WaitingConfirmations" | "WaitingBroadcast" | "Settled" | "Failed" | null;
            failure?: components["schemas"]["RefreshFailure"] | null;
        };
        RgbInvoiceRequest: {
            /** @example 1 */
            min_confirmations: number;
            /** @example rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8 */
            asset_id?: string | null;
            /** @example null */
            assignment?: components["schemas"]["Assignment"] | null;
            /** @example 1695811760 */
            expiration_timestamp?: number;
            /** @example false */
            witness: boolean;
            /**
             * @example [
             *       "rpc://127.0.0.1:3000/json-rpc"
             *     ]
             */
            transport_endpoints: string[];
        };
        RgbInvoiceResponse: {
            /** @example bcrt:utxob:cbgHUJ4e-7QyKY4U-Jsj5AZw-oI0gxZh-7fxQY2_-tFFUAZN-4CgpX */
            recipient_id: string;
            /** @example rgb:~/~/~/bcrt:utxob:cbgHUJ4e-7QyKY4U-Jsj5AZw-oI0gxZh-7fxQY2_-tFFUAZN-4CgpX?expiry=1695811760&endpoints=rpc://127.0.0.1:3000/json-rpc */
            invoice: string;
            /** @example 1695811760 */
            expiration_timestamp: number;
            /** @example 1 */
            batch_transfer_idx: number;
        };
        SendBtcRequest: {
            /** @example 16900 */
            amount: number;
            /** @example bcrt1qwxht5tut39dws8tjcf649tp908r8fr2j75c94k */
            address: string;
            /** @example 5 */
            fee_rate: number;
            /** @example false */
            skip_sync: boolean;
        };
        SendBtcResponse: {
            /** @example 7c2c95b9c2aa0a7d140495b664de7973b76561de833f0dd84def3efa08941664 */
            txid: string;
        };
        SendPaymentRequest: {
            /** @example lnbcrt30u1pjv6yzndqud3jxktt5w46x7unfv9kz6mn0v3jsnp4qdpc280eur52luxppv6f3nnj8l6vnd9g2hnv3qv6mjhmhvlzf6327pp5tjjasx6g9dqptea3fhm6yllq5wxzycnnvp8l6wcq3d6j2uvpryuqsp5l8az8x3g8fe05dg7cmgddld3da09nfjvky8xftwsk4cj8p2l7kfq9qyysgqcqpcxqzdylzlwfnkyw3jv344x4rzwgkk53ng0fhxy5rdduk4g5tpvea8xa6rfckkza35va28xjn2tqkhgarcxep5umm4x5k56wfcdvu95eq7qzp20vrl4xz76syapsa3c09j7lg5gerkaj63llj0ark7ph8hfketn6fkqzm8laf66dhsncm23wkwm5l5377we9e8lnlknnkwje5eefkccusqm6rqt8 */
            invoice: string;
            /** @example 3000000 */
            amt_msat?: number | null;
            /** @example rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8 */
            asset_id?: string | null;
            /** @example 100 */
            asset_amount?: number | null;
        };
        SendPaymentResponse: {
            /** @example 3febfae1e68b190c15461f4c2a3290f9af1dae63fd7d620d2bd61601869026cd */
            payment_id: string;
            /** @example 3febfae1e68b190c15461f4c2a3290f9af1dae63fd7d620d2bd61601869026cd */
            payment_hash?: string | null;
            /** @example 777a7756c620868199ed5fdc35bee4095b5709d543e5c2bf0494396bf27d2ea2 */
            payment_secret?: string | null;
            status: components["schemas"]["HTLCStatus"];
        };
        SendRgbRequest: {
            /** @example false */
            donation: boolean;
            /** @example 5 */
            fee_rate: number;
            /** @example 1 */
            min_confirmations: number;
            /** @example 1695811760 */
            expiration_timestamp?: number;
            /**
             * @example {
             *       "rgb:CJkb4YZw-jRiz2sk-~PARPio-wtVYI1c-XAEYCqO-wTfvRZ8": [
             *         {
             *           "recipient_id": "utxob:2FjRqgQ-eEWCVHY5-zmpFtYzT-gGm3MdR-sTnxNcS-7RtUbY9-4NYuuh",
             *           "assignment": {
             *             "type": "Fungible",
             *             "value": 400
             *           },
             *           "transport_endpoints": [
             *             "rpc://127.0.0.1:3000/json-rpc"
             *           ]
             *         },
             *         {
             *           "recipient_id": "utxob:3GkRrhR-fFXDLIZ6-0anqGuzU-hHn4NeS-tUoyOdT-8SuVcZ0-5OZvvi",
             *           "assignment": {
             *             "type": "Fungible",
             *             "value": 200
             *           },
             *           "transport_endpoints": [
             *             "rpc://127.0.0.1:3000/json-rpc"
             *           ]
             *         }
             *       ],
             *       "rgb:d8qDVS5X-ICVG2uM-CPr3yO4-lfBhgjt-7FN1EPE-ApY1LcM": [
             *         {
             *           "recipient_id": "utxob:4HlSsiS-gGYEMKA7-1borHvaV-iIo5OfT-uVpzPeU-9TvWdA1-6PAwwj",
             *           "assignment": {
             *             "type": "Fungible",
             *             "value": 100
             *           },
             *           "transport_endpoints": [
             *             "rpc://127.0.0.1:3000/json-rpc"
             *           ]
             *         }
             *       ]
             *     }
             */
            recipient_map: {
                [key: string]: components["schemas"]["Recipient"][];
            };
        };
        SendRgbResponse: {
            /** @example 7c2c95b9c2aa0a7d140495b664de7973b76561de833f0dd84def3efa08941664 */
            txid: string;
        };
        Transfer: {
            /** @example 1 */
            idx: number;
            /** @example 1691160765 */
            created_at: number;
            /** @example 1691162674 */
            updated_at: number;
            status: components["schemas"]["TransferStatus"];
            requested_assignment?: components["schemas"]["Assignment"] | null;
            assignments: components["schemas"]["Assignment"][];
            kind: components["schemas"]["TransferKind"];
            /** @example 7c2c95b9c2aa0a7d140495b664de7973b76561de833f0dd84def3efa08941664 */
            txid?: string | null;
            /** @example 61qsVbWtkNmU54F2i6qtB9uSmEGsPoaeypCi5uC5uctZ */
            recipient_id?: string | null;
            /** @example 61qsVbWtkNmU54F2i6qtB9uSmEGsPoaeypCi5uC5ucX */
            proxy_recipient_id?: string | null;
            /** @example efed66f5309396ff43c8a09941c8103d9d5bbffd473ad9f13013ac89fb6b4671:0 */
            receive_utxo?: string | null;
            /** @example null */
            change_utxo?: string | null;
            /** @example 1691171612 */
            expiration_timestamp?: number | null;
            transport_endpoints: components["schemas"]["TransferTransportEndpoint"][];
        };
        /**
         * @example ReceiveBlind
         * @enum {string}
         */
        TransferKind: "Issuance" | "ReceiveBlind" | "ReceiveWitness" | "Send" | "Inflation" | "Burn";
        /** @enum {string} */
        TransferStatus: "Initiated" | "WaitingCounterparty" | "WaitingSafeHeight" | "WaitingConfirmations" | "WaitingBroadcast" | "Settled" | "Failed";
        TransferTransportEndpoint: {
            /** @example http://127.0.0.1:3000/json-rpc */
            endpoint: string;
            transport_type: components["schemas"]["TransportType"];
            /** @example false */
            used: boolean;
        };
        /** @enum {string} */
        TransportType: "JsonRpc";
        WitnessData: {
            /** @example 1000 */
            amount_sat: number;
            /** @example 439017309 */
            blinding?: number | null;
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export type operations = Record<string, never>;
